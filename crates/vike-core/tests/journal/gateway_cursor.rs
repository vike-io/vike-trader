//! The JOURNAL CURSOR (journal v18): an event that came over a gateway daemon's trade link is
//! journaled as ONE `Ingest::GatewayEvent` record carrying its ring position, each `Snapshot` carries
//! where every ring stood, and the boot restore hands those positions back as the resume point of
//! the next trade session.
//!
//! Gated here, because only a journal on disk shows them: ONE record per event (never an event and
//! a cursor apart: a crash cannot separate them, so a funding payment, which has no id and no
//! dedup, cannot fold twice across a restart); the per-account cursor rule (the larger `seq` inside
//! an epoch, the later arrival across epochs); a cursor surviving a `Snapshot` and the prune that
//! deletes the segment holding its last event; a restarted core carrying the restored cursors into
//! its next `Snapshot`; a run with no gateway event writing no trace of the feature; and a v17 journal
//! restoring as before. The fold's other consumers (the tearsheet's fills, the crash tail's
//! terminal facts, the materializer, the relay) are gated beside the code each one is.

use crate::scratch::Scratch;
use std::path::Path;

use vike_core::replay::{RestoreOutcome, restore_for_boot};
use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_execution::{GatewayEvent, Ingest};
use vike_journal::{Journal, JournalFileConfig, JournalRecord, JournalSnapshotParts};
use vike_model::EventRingCursor;
use vike_model::events::{Event, FundingEvent, PositionSide};

use crate::kit::engines::sim_engine;
use crate::kit::events::sim_bare_fill;
use crate::kit::journal::{build_source_journal, counter_clock, journal_at, prune_closed};

/// A segment's header length (`vike_journal`'s private `format::HEADER`): magic, version, first seq.
const HEADER: usize = 16;

fn cursor(gateway_epoch: u64, seq: u64) -> EventRingCursor {
    EventRingCursor { gateway_epoch, sequence: seq, timestamp_ms: seq as i64 * 10 }
}

fn gateway(event: Event, route_key: &str, c: EventRingCursor) -> GatewayEvent {
    GatewayEvent { event, route_key: route_key.into(), cursor: c }
}

/// A funding payment on `sim`/`BTCUSDT`: not in venue history, carries no id, and
/// `Account::apply_funding` is a bare `+=`, so folding it twice is the double-count a cursor in the
/// SAME record as the event exists to rule out.
fn funding(amount: f64) -> Event {
    Event::Funding(FundingEvent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        position_side: PositionSide::Both,
        funding_rate: 0.0001,
        amount,
        mark_price: None,
        timestamp: 1,
        route_key: None,
        currency: None,
    })
}

/// The cursors the LAST `Snapshot` of the journal at `dir` carries.
fn last_snap_cursors(dir: &Path) -> Vec<(String, EventRingCursor)> {
    Journal::read_all(dir)
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snapshot { gateway_cursors, .. } => Some(gateway_cursors),
            _ => None,
        })
        .expect("a clean stop writes an exit Snapshot")
}

/// The `funding_paid` of the first engine of the LAST `Snapshot`.
fn last_snap_funding(dir: &Path) -> f64 {
    Journal::read_all(dir)
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snapshot { engines, .. } => Some(engines[0].account.funding_paid),
            _ => None,
        })
        .expect("a clean stop writes an exit Snapshot")
}

/// Run a core over a fresh journal at `dir`, deliver `events` through its gateway door in order,
/// and stop it cleanly.
fn run_core(dir: &Path, snapshot_every: u64, segment_bytes: u64, events: Vec<GatewayEvent>) {
    let handle = spawn_core(
        sim_engine(),
        CoreConfig {
            seed_cash: 1_000.0,
            clock: counter_clock(),
            order_client_id_session: Some(("cafef00d".into(), 0)),
            journal: Some(journal_at(dir, segment_bytes, snapshot_every)),
            ..CoreConfig::default()
        },
    );
    let sender = handle.event_sender();
    for g in events {
        sender.blocking_send_gateway(g).unwrap();
    }
    handle.shutdown_and_join();
}

/// **ONE record per event, and the cursor rides the `Snapshot`.** Three gateway events across two
/// accounts reach the journal as exactly three `GatewayEvent` records and NO plain `Event` record
/// beside them, the funding payment is folded once, and the exit `Snapshot` holds each account's last
/// cursor in the order the accounts first appeared.
#[test]
fn a_gateway_event_is_one_journal_record_and_the_snap_carries_the_cursors() {
    let dir = Scratch::reserved("gw-cursor-one-record");
    run_core(
        &dir,
        1_000,
        1 << 20,
        vec![
            gateway(sim_bare_fill("g1", 1.0, 100.0), "sim", cursor(7, 1)),
            gateway(funding(-1.25), "sim", cursor(7, 2)),
            gateway(sim_bare_fill("g2", 1.0, 100.0), "sim#alt", cursor(7, 5)),
        ],
    );

    let recs = Journal::read_all(&dir).unwrap();
    let gateway_commands = recs
        .iter()
        .filter(|r| matches!(r, JournalRecord::Command { message: Ingest::GatewayEvent(_), .. }))
        .count();
    let plain_commands = recs
        .iter()
        .filter(|r| matches!(r, JournalRecord::Command { message: Ingest::Event(_), .. }))
        .count();
    assert_eq!(gateway_commands, 3, "one record per event");
    assert_eq!(
        plain_commands, 0,
        "never an Event beside its GatewayEvent: that is how a cursor drifts"
    );
    assert_eq!(
        last_snap_cursors(&dir),
        vec![("sim".to_string(), cursor(7, 2)), ("sim#alt".to_string(), cursor(7, 5))],
        "each account's LAST cursor, in first-seen order"
    );
    assert!((last_snap_funding(&dir) - -1.25).abs() < 1e-12, "the funding payment folded once");
}

/// **The restore hands back the maximum cursor per account** -- the `Snapshot`'s, advanced by the tail --
/// and the funding event of the crash tail is folded EXACTLY ONCE: the same record that carries
/// the event carries its cursor, so a resume asking for `seq + 1` cannot re-deliver it.
///
/// The tail is appended by hand after a clean stop (a core killed before its exit `Snapshot` leaves the
/// same shape): account `sim` advances to `seq 9`, is then offered a straggler at `seq 4`, which
/// must not rewind it, and then a NEW epoch, which wins whatever its `seq`. Account `sim#alt` has no
/// engine in the `Snapshot` (the core ran one engine): its tail event reached no engine, so its cursor stays
/// the `Snapshot`'s (`an_account_armed_after_the_snap_gets_no_cursor_from_the_tail`).
#[test]
fn the_restore_takes_the_last_cursor_per_account_and_folds_the_tail_funding_once() {
    let dir = Scratch::reserved("gw-cursor-restore");
    let tmp = Scratch::created("gw-cursor-restore-tmp");
    run_core(
        &dir,
        1_000,
        1 << 20,
        vec![
            gateway(sim_bare_fill("g1", 1.0, 100.0), "sim", cursor(7, 1)),
            gateway(funding(-1.25), "sim", cursor(7, 2)),
            gateway(sim_bare_fill("g2", 1.0, 100.0), "sim#alt", cursor(7, 5)),
        ],
    );
    {
        let mut j =
            Journal::open(&dir, JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 })
                .unwrap();
        let tail = [
            gateway(funding(-0.75), "sim", cursor(7, 9)),
            gateway(sim_bare_fill("g3", 1.0, 100.0), "sim", cursor(7, 4)),
            gateway(sim_bare_fill("g4", 1.0, 100.0), "sim", cursor(8, 1)),
            gateway(sim_bare_fill("g5", 1.0, 100.0), "sim#alt", cursor(8, 1)),
        ];
        for (i, g) in tail.into_iter().enumerate() {
            j.append_command(1_000 + i as i64, &Ingest::GatewayEvent(Box::new(g))).unwrap();
        }
        j.flush().unwrap();
    }

    let restore =
        restore_for_boot(Some(&journal_at(&dir, 1 << 20, 1_000)), &tmp, 42, &[]).expect("readable");
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    assert_eq!(
        restore.gateway_cursors(),
        [("sim".to_string(), cursor(8, 1)), ("sim#alt".to_string(), cursor(7, 5))],
        "the larger seq inside an epoch (a straggler cannot rewind it), the later arrival across"
    );
    let funding_paid = restore.restored_engines()[0].account.funding_paid;
    assert!(
        (funding_paid - (-1.25 + -0.75)).abs() < 1e-12,
        "the checkpoint's funding plus the tail's, each ONCE: {funding_paid}"
    );
}

/// **A gateway FILL the core folded whose record never reached the file is asked for again.**
/// With the asynchronous journal (decision 0279) a process that
/// dies within the write thread's lag (about a millisecond, the owner accepted it for non-order
/// records on 2026-11-12) leaves a folded fill with no record. Its shape on disk is the segment
/// WITHOUT that frame (zeros where the write thread would have put it). The restore then hands back
/// the cursor of the last record that IS there, so the next trade session resumes BEFORE the lost
/// fill and the daemon replays it: `vike-gatewayhub-client`'s
/// `a_process_restart_resumes_from_the_journals_cursor` pins that a resume delivers every position
/// strictly after the journal's cursor. (A ring that no longer holds it answers `ReplayUnavailable`,
/// which triggers a reconcile pass: that client file's next test.)
#[test]
fn a_gateway_fill_whose_record_never_reached_the_file_is_resumed_from_before_it() {
    let dir = Scratch::reserved("gw-cursor-lost-fill");
    let tmp = Scratch::created("gw-cursor-lost-fill-tmp");
    run_core(
        &dir,
        1_000,
        1 << 20,
        vec![
            gateway(sim_bare_fill("g1", 1.0, 100.0), "sim", cursor(7, 1)),
            gateway(sim_bare_fill("g2", 1.0, 100.0), "sim", cursor(7, 2)),
        ],
    );
    // The fill at seq 3 is journaled after the exit `Snapshot`, as a live core would, then lost: zeroed
    // from its frame on, which is what a process death before its `pwrite` leaves.
    let seg = dir.join("journal-00000000.vjl");
    let before = {
        let b = std::fs::read(&seg).unwrap();
        frames_end(&b)
    };
    {
        let mut j =
            Journal::open(&dir, JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 })
                .unwrap();
        let lost = gateway(sim_bare_fill("g3", 1.0, 100.0), "sim", cursor(7, 3));
        j.append_command(1_000, &Ingest::GatewayEvent(Box::new(lost))).unwrap();
    }
    let mut bytes = std::fs::read(&seg).unwrap();
    assert!(frames_end(&bytes) > before, "precondition: the fill's record was written");
    bytes[before..].fill(0);
    std::fs::write(&seg, &bytes).unwrap();

    let restore =
        restore_for_boot(Some(&journal_at(&dir, 1 << 20, 1_000)), &tmp, 42, &[]).expect("readable");
    assert_eq!(
        restore.gateway_cursors(),
        [("sim".to_string(), cursor(7, 2))],
        "the resume point is the last fill the journal holds, so seq 3 is delivered again"
    );
}

/// The end of the whole frames of a segment's bytes.
fn frames_end(b: &[u8]) -> usize {
    let mut at = HEADER;
    while at + 8 <= b.len() {
        let len = u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize;
        if len == 0 || at + 8 + len > b.len() {
            break;
        }
        at += 8 + len;
    }
    at
}

/// **An account that stayed quiet keeps its cursor through a `Snapshot` and the prune.** Account `old`
/// speaks once, early; then `sim` (the account the core has an engine for) fills the journal across many segments and cadence `Snapshot`s.
/// After the prune that deletes the early segments, no `GatewayEvent` for `old` is left to read --
/// its cursor lives only in the latest `Snapshot`, and the restore must still return it.
#[test]
fn a_quiet_account_keeps_its_cursor_through_the_snap_and_the_prune() {
    let dir = Scratch::reserved("gw-cursor-prune");
    let tmp = Scratch::created("gw-cursor-prune-tmp");
    let mut events = vec![gateway(sim_bare_fill("old-1", 1.0, 100.0), "old", cursor(3, 11))];
    for i in 0..400u64 {
        let tid = format!("busy-{i}");
        events.push(gateway(sim_bare_fill(&tid, 1.0, 100.0), "sim", cursor(3, 1 + i)));
    }
    // 16 KiB segments: the `Snapshot` grows with every fill it dedups, and as msgpack (format 34) it outgrew a 4 KiB
    // segment at about the 340th (measured on a lane), which no frame may.
    run_core(&dir, 8, 16 * 1024, events);

    prune_closed(&dir);
    let left = Journal::read_all(&dir).unwrap();
    assert!(
        !left.iter().any(|r| matches!(
            r,
            JournalRecord::Command { message: Ingest::GatewayEvent(g), .. } if g.route_key.as_str() == "old"
        )),
        "precondition: the prune really took the only record that spoke for `old`"
    );

    let restore =
        restore_for_boot(Some(&journal_at(&dir, 16 * 1024, 8)), &tmp, 42, &[]).expect("readable");
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    let cursors = restore.gateway_cursors();
    assert!(
        cursors.contains(&("old".to_string(), cursor(3, 11))),
        "the quiet account: {cursors:?}"
    );
    assert!(cursors.contains(&("sim".to_string(), cursor(3, 400))), "the busy one: {cursors:?}");
}

/// **A restarted core carries the restored cursors into its next `Snapshot`.** The core only holds
/// them; the proof is the `Snapshot` it writes after the restart, where the account that stayed silent
/// keeps its cursor and the one that spoke has advanced.
#[test]
fn a_restarted_core_keeps_the_restored_cursors_in_its_next_snap() {
    let dir = Scratch::reserved("gw-cursor-restart");
    let tmp = Scratch::created("gw-cursor-restart-tmp");
    run_core(
        &dir,
        1_000,
        1 << 20,
        vec![
            gateway(sim_bare_fill("a1", 1.0, 100.0), "sim", cursor(7, 3)),
            gateway(sim_bare_fill("b1", 1.0, 100.0), "sim#alt", cursor(7, 8)),
        ],
    );

    let restore =
        restore_for_boot(Some(&journal_at(&dir, 1 << 20, 1_000)), &tmp, 42, &[]).expect("readable");
    let mut config = CoreConfig {
        seed_cash: 1_000.0,
        clock: counter_clock(),
        journal: Some(journal_at(&dir, 1 << 20, 1_000)),
        ..CoreConfig::default()
    };
    restore.apply_to(&mut config);
    assert_eq!(config.gateway_cursors.len(), 2, "the restore hands the cursors to the config");
    let handle = spawn_core(sim_engine(), config);
    handle
        .event_sender()
        .blocking_send_gateway(gateway(sim_bare_fill("a2", 1.0, 100.0), "sim", cursor(7, 4)))
        .unwrap();
    handle.shutdown_and_join();

    let mut got = last_snap_cursors(&dir);
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        got,
        vec![("sim".to_string(), cursor(7, 4)), ("sim#alt".to_string(), cursor(7, 8))],
        "`sim` advanced, `sim#alt` stayed quiet and kept its restored cursor"
    );
}

/// **A run that never receives a gateway event writes no trace of the feature**: no `GatewayEvent`
/// record and no `gateway_cursors` key on any `Snapshot`, in any segment, so its journal bytes are the
/// bytes it wrote before v18 (only the segment header's version moves).
#[test]
fn a_run_with_no_gateway_event_writes_no_trace_of_one() {
    let dir = Scratch::reserved("gw-cursor-none");
    build_source_journal(&dir);
    let mut segments = 0;
    for e in std::fs::read_dir(&*dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|s| s.to_str()) != Some("vjl") {
            continue;
        }
        segments += 1;
        let bytes = std::fs::read(&p).unwrap();
        for needle in [&b"gateway"[..], &b"GatewayEvent"[..]] {
            assert!(
                !bytes.windows(needle.len()).any(|w| w == needle),
                "{} holds `{}`",
                p.display(),
                String::from_utf8_lossy(needle)
            );
        }
    }
    assert!(segments > 0, "the source scenario wrote a segment");
}

/// **A journal with no gateway event restores with no cursors.** Until format 30 this restamped the
/// journal to v17 (a run with no gateway event was byte-for-byte what a v17 build wrote, the header
/// aside); formats 30 and 31 raised the floor to 31, so an older header is refused and the journal is the floor's own.
#[test]
fn a_journal_with_no_gateway_event_restores_with_no_cursors() {
    let dir = Scratch::reserved("gw-cursor-v17");
    let tmp = Scratch::created("gw-cursor-v17-tmp");
    build_source_journal(&dir);
    for e in std::fs::read_dir(&*dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|s| s.to_str()) != Some("vjl") {
            continue;
        }
        let bytes = std::fs::read(&p).unwrap();
        assert!(bytes.len() > HEADER);
        // the floor's own stamp (format 34: every older step, the execution report's 33 included, is below it)
        let stamp = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        assert_eq!(stamp, 34, "the floor's own stamp");
    }
    let restore = restore_for_boot(Some(&JournalConfig::at(dir.to_path_buf())), &tmp, 7, &[])
        .expect("readable");
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    assert!(!restore.restored_engines().is_empty(), "it restores as it always did");
    assert!(restore.gateway_cursors().is_empty(), "and holds no cursor");
}

fn link_update(up: bool) -> vike_execution::TradeLinkUpdate {
    vike_execution::TradeLinkUpdate {
        route_key: "sim".into(),
        venue: "sim".into(),
        up,
        at_timestamp_ms: 5,
    }
}

/// **A trade-link transition is NOT journaled**: it is a transport fact about this process's
/// connection, not order state. Down/up either side of a gateway event leaves exactly the
/// gateway event's record in the journal and nothing for the link.
#[test]
fn an_execution_link_transition_is_never_journaled() {
    let dir = Scratch::reserved("gw-cursor-exec-link-unjournaled");
    let handle = spawn_core(
        sim_engine(),
        CoreConfig {
            seed_cash: 1_000.0,
            clock: counter_clock(),
            order_client_id_session: Some(("cafef00d".into(), 0)),
            journal: Some(journal_at(&dir, 1 << 20, 1_000)),
            ..CoreConfig::default()
        },
    );
    let sender = handle.event_sender();
    sender.execution_link(link_update(false)).unwrap();
    sender
        .blocking_send_gateway(gateway(sim_bare_fill("g1", 1.0, 100.0), "sim", cursor(7, 1)))
        .unwrap();
    sender.execution_link(link_update(true)).unwrap();
    handle.shutdown_and_join();

    let recs = Journal::read_all(&dir).unwrap();
    let commands: Vec<&Ingest> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::Command { message: msg, .. } => Some(msg),
            _ => None,
        })
        .collect();
    assert!(
        commands.iter().all(|m| !matches!(m, Ingest::TradeLink(_))),
        "an ExecLink was journaled: {commands:?}"
    );
    assert_eq!(
        commands.iter().filter(|m| matches!(m, Ingest::GatewayEvent(_))).count(),
        1,
        "...and the gateway event beside it still is"
    );
}

/// **A foreign journal that DOES hold an `ExecLink` record restores as if it did not.** Nothing in
/// this build writes one, but the refold's `ExecLink` arm is a named one and must skip the record
/// rather than replay it or panic: the cursors and the tail's funding are exactly what they are
/// without it.
#[test]
fn a_journal_holding_an_execution_link_record_restores_unchanged() {
    let dir = Scratch::reserved("gw-cursor-exec-link-foreign");
    let tmp = Scratch::created("gw-cursor-exec-link-foreign-tmp");
    run_core(
        &dir,
        1_000,
        1 << 20,
        vec![gateway(sim_bare_fill("g1", 1.0, 100.0), "sim", cursor(7, 1))],
    );
    {
        let mut j =
            Journal::open(&dir, JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 })
                .unwrap();
        j.append_command(1_000, &Ingest::TradeLink(Box::new(link_update(false)))).unwrap();
        j.append_command(
            1_001,
            &Ingest::GatewayEvent(Box::new(gateway(funding(-0.5), "sim", cursor(7, 2)))),
        )
        .unwrap();
        j.append_command(1_002, &Ingest::TradeLink(Box::new(link_update(true)))).unwrap();
        j.flush().unwrap();
    }
    let restore =
        restore_for_boot(Some(&journal_at(&dir, 1 << 20, 1_000)), &tmp, 42, &[]).expect("readable");
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    assert_eq!(restore.gateway_cursors(), [("sim".to_string(), cursor(7, 2))]);
    let funding_paid = restore.restored_engines()[0].account.funding_paid;
    assert!((funding_paid - -0.5).abs() < 1e-12, "the tail's funding, once: {funding_paid}");
}

/// The refold's tail loop is an EXHAUSTIVE match that names `ExecLink`: a `_ =>` arm would let the
/// next `Ingest` variant be dropped from a replay without a compile error. The skip is
/// indistinguishable at run time from a wildcard's, so this guard reads the source.
#[test]
fn the_refold_tail_match_names_execution_link_and_has_no_wildcard_arm() {
    let src = include_str!("../../src/replay/refold.rs");
    let from = src.find("for (sequence, _, msg) in tail {").expect("the tail loop exists");
    let body = &src[from..];
    let body = &body[..body.find("\n    }\n").expect("the tail loop ends")];
    assert!(body.contains("Ingest::TradeLink(_)"), "ExecLink must be classified by name");
    assert!(!body.contains("_ =>"), "the tail match must stay exhaustive: no wildcard arm");
}

/// A crashed tradehub's journal: a `Snapshot` of one engine that holds the cursor `(epoch 7, seq 10)`
/// for its account, then a tail the dead core had journaled and not yet folded into a `Snapshot`: a
/// funding payment (seq 11) and two fills (seq 12, 13). The engine is the venue's sole account
/// (`label` None: the refold can run) or a LABELLED one (`Some`: the refold refuses it, the shape
/// of the multi-account gateway box). Returns the account's route key.
fn crashed_gateway_journal(dir: &Path, label: Option<&str>) -> String {
    let mut snap = sim_engine().snapshot_state();
    let key = match label {
        Some(l) => {
            snap.route_key = Some(format!("sim#{l}"));
            format!("sim#{l}")
        }
        None => "sim".to_string(),
    };
    let engines = vec![snap];
    let hash = vike_execution::state_hash(&engines);
    let held = [(key.clone(), cursor(7, 10))];
    let mut j =
        Journal::open(dir, JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 }).unwrap();
    let books = JournalSnapshotParts { gateway_cursors: &held, ..JournalSnapshotParts::default() };
    j.append_snap(10, &engines, "cafef00d", 0, 0, books, hash).unwrap();
    let tail = [
        gateway(funding(-1.25), &key, cursor(7, 11)),
        gateway(sim_bare_fill("t12", 1.0, 100.0), &key, cursor(7, 12)),
        gateway(sim_bare_fill("t13", 1.0, 100.0), &key, cursor(7, 13)),
    ];
    for (i, g) in tail.into_iter().enumerate() {
        j.append_command(11 + i as i64, &Ingest::GatewayEvent(Box::new(g))).unwrap();
    }
    j.flush().unwrap();
    key
}

/// **A snap-only restore hands back the cursor the engines are AT, not the one the tail reached.**
/// The refold cannot run (the scratch root is a FILE, the recipe `boot_restore.rs` uses), so no tail
/// event is folded into any engine: the funding payment and both fills are NOT in the restored
/// state. A cursor of `(7, 13)` would tell the gateway they were applied, and it would never send
/// them again -- a funding payment has no id and no other way back. The cursor must stay `(7, 10)`.
/// The base holds a LABELLED engine, which used to be the refusal itself; it is the harder case now
/// that the refold runs on one (`a_labelled_engine_refolds_its_gateway_tail`).
#[test]
fn a_snap_only_restore_hands_back_the_cursor_its_engines_are_at() {
    let dir = Scratch::reserved("gw-cursor-snaponly");
    let tmp = Scratch::created("gw-cursor-snaponly-tmp");
    let key = crashed_gateway_journal(&dir, Some("ALT"));
    let not_a_dir = tmp.join("not-a-dir");
    std::fs::write(&not_a_dir, b"a file").unwrap();

    let restore = restore_for_boot(Some(&journal_at(&dir, 1 << 20, 1_000)), &not_a_dir, 42, &[])
        .expect("readable");
    let RestoreOutcome::SnapOnly { why } = restore.outcome() else {
        panic!("a scratch root that is a file cannot refold, got {:?}", restore.outcome());
    };
    assert!(why.contains("not-a-dir"), "the refold named the root it was given: {why}");
    let account = &restore.restored_engines()[0].account;
    assert_eq!(account.funding_paid, 0.0, "precondition: the tail's funding was NOT folded");
    assert!(account.positions.is_empty(), "precondition: nor were the tail's fills");
    assert_eq!(
        restore.gateway_cursors(),
        [(key, cursor(7, 10))],
        "the resume asks for seq 11: the engines have not seen 11, 12 or 13"
    );
}

/// **A LABELLED engine's gateway tail is refolded, and its cursor advances over what was folded.**
/// The same crashed journal as above over a labelled account (`sim#ALT`, the venue's only engine):
/// the restore used to read the latest `Snapshot` alone for any base holding one (decision 0127 verdict 5),
/// so the funding payment and both fills were lost to the restored state until a reconcile pass found
/// them. They are folded now, and the sole-account control agrees with it number for number.
#[test]
fn a_labelled_engine_refolds_its_gateway_tail() {
    let dir = Scratch::reserved("gw-cursor-labelled-refold");
    let sole_dir = Scratch::reserved("gw-cursor-labelled-refold-sole");
    let tmp = Scratch::created("gw-cursor-labelled-refold-tmp");
    let key = crashed_gateway_journal(&dir, Some("ALT"));
    let sole_key = crashed_gateway_journal(&sole_dir, None);

    let labelled =
        restore_for_boot(Some(&journal_at(&dir, 1 << 20, 1_000)), &tmp, 42, &[]).expect("readable");
    assert_eq!(labelled.outcome(), &RestoreOutcome::Full, "{:?}", labelled.notes());
    let sole = restore_for_boot(Some(&journal_at(&sole_dir, 1 << 20, 1_000)), &tmp, 42, &[])
        .expect("readable");
    assert_eq!(sole.outcome(), &RestoreOutcome::Full, "{:?}", sole.notes());

    let (l, s) = (&labelled.restored_engines()[0], &sole.restored_engines()[0]);
    assert_eq!(l.route_key.as_deref(), Some("sim#ALT"), "the labelled engine is the one restored");
    assert!((l.account.funding_paid - -1.25).abs() < 1e-12, "{}", l.account.funding_paid);
    assert_eq!(
        l.account.positions.iter().map(|(_, p)| p.size).sum::<f64>(),
        2.0,
        "both tail fills landed in the labelled engine"
    );
    assert_eq!(l.account.funding_paid, s.account.funding_paid, "same fold as the sole account");
    assert_eq!(labelled.gateway_cursors(), [(key, cursor(7, 13))], "the cursor follows the fold");
    assert_eq!(sole.gateway_cursors(), [(sole_key, cursor(7, 13))]);
}

/// **A Full restore hands back no cursor for an account the `Snapshot` holds no engine for.** The base
/// `Snapshot` knows one engine, `sim`. The dead core had since armed a second account, `later`, which
/// traded (two fills) before the crash: its events sit in the tail, but the refold's replay core has
/// only the `Snapshot`'s engines, so they reached NO engine. A cursor of `later`'s `(3, 2)` would tell the
/// gateway they were applied, and it would never send them again; the restore must return `sim`'s
/// advanced cursor and nothing for `later` (its session opens fresh and a reconcile recovers what it
/// can). The control is `sim` itself, which the refold DID fold.
#[test]
fn an_account_armed_after_the_snap_gets_no_cursor_from_the_tail() {
    let dir = Scratch::reserved("gw-cursor-armed-after");
    let tmp = Scratch::created("gw-cursor-armed-after-tmp");
    let key = crashed_gateway_journal(&dir, None);
    {
        let mut j =
            Journal::open(&dir, JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 })
                .unwrap();
        let tail = [
            gateway(sim_bare_fill("l1", 1.0, 100.0), "later", cursor(3, 1)),
            gateway(sim_bare_fill("l2", 1.0, 100.0), "later", cursor(3, 2)),
        ];
        for (i, g) in tail.into_iter().enumerate() {
            j.append_command(2_000 + i as i64, &Ingest::GatewayEvent(Box::new(g))).unwrap();
        }
        j.flush().unwrap();
    }

    let restore =
        restore_for_boot(Some(&journal_at(&dir, 1 << 20, 1_000)), &tmp, 42, &[]).expect("readable");
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    assert_eq!(
        restore.gateway_cursors(),
        [(key, cursor(7, 13))],
        "`sim` advanced over the tail it folded; `later` has no engine, so no cursor"
    );
}

/// **A redelivered prefix is neither journaled nor folded twice.** Three events (a fill, a funding
/// payment, a fill) are delivered, then the same three again with their cursors (a reader that
/// reconnected with events still in flight), then one NEW event. The journal holds four
/// `GatewayEvent` records, the funding payment is folded once, and the exit `Snapshot` holds the last
/// cursor.
#[test]
fn a_redelivered_prefix_is_neither_journaled_nor_folded_twice() {
    let dir = Scratch::reserved("gw-cursor-redelivery");
    let story = || {
        vec![
            gateway(sim_bare_fill("r1", 1.0, 100.0), "sim", cursor(7, 1)),
            gateway(funding(-1.25), "sim", cursor(7, 2)),
            gateway(sim_bare_fill("r2", 1.0, 100.0), "sim", cursor(7, 3)),
        ]
    };
    let mut events = story();
    events.extend(story());
    events.push(gateway(funding(-0.5), "sim", cursor(7, 4)));
    run_core(&dir, 1_000, 1 << 20, events);

    let recs = Journal::read_all(&dir).unwrap();
    let journaled: Vec<u64> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::Command { message: Ingest::GatewayEvent(g), .. } => {
                Some(g.cursor.sequence)
            }
            _ => None,
        })
        .collect();
    assert_eq!(journaled, [1, 2, 3, 4], "the redelivered prefix never reached the journal");
    assert!(
        (last_snap_funding(&dir) - (-1.25 + -0.5)).abs() < 1e-12,
        "each funding payment folded once: {}",
        last_snap_funding(&dir)
    );
    assert_eq!(last_snap_cursors(&dir), vec![("sim".to_string(), cursor(7, 4))]);
}
