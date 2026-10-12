//! Task 7 gate: the `replay_journal` support CLI, invoked as a real child process — the
//! `fake_jforex_bridge` pattern (`env!("CARGO_BIN_EXE_replay_journal")` + `std::process::Command`).
//!
//! The journal-building scenario is the one `replay_fence.rs` fences, shared through the test kit
//! (`crates/vike-core/tests/support/journal.rs`'s `build_source_journal`) rather than duplicated as
//! it was while the suites had no common module. Exercises all three CLI exit paths: 0 (fence
//! verified), 1 (`replay_offline` returns `Err`), 2 (usage error — no argv).

use crate::scratch::Scratch;
use std::process::Command as OsCommand;

use vike_journal::{CommandJournal, JournalFileConfig, JournalRecord};

use crate::kit::journal::{build_source_journal, copy_journal_files};

/// Path of the `replay_journal` bin (cargo builds crate bins for integration tests).
const REPLAY_JOURNAL: &str = env!("CARGO_BIN_EXE_replay_journal");

/// A scratch journal directory, removed when the returned guard drops. The journal's own `open`
/// calls `create_dir_all`, so the path is RESERVED rather than created. Hold the guard for the
/// whole test — see `crates/vike-core/src/scratch.rs` for the leak this closed.
fn unique_dir(tag: &str) -> Scratch {
    Scratch::reserved(&format!("cli-{tag}"))
}

/// POSITIVE: a valid journal replays clean through the real bin — exit 0, stdout carries the
/// summary lines (records/snaps/hash + the one engine's venue/symbol/order/position/balance).
#[test]
fn replay_journal_reports_success_on_valid_journal() {
    let dir = unique_dir("ok");
    build_source_journal(&dir);

    let out = OsCommand::new(REPLAY_JOURNAL).arg(&dir).output().expect("spawn replay_journal");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}; stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("journal records"), "stdout missing records line: {stdout}");
    assert!(stdout.contains("snaps verified"), "stdout missing snaps line: {stdout}");
    assert!(stdout.contains("final state hash"), "stdout missing hash line: {stdout}");
    assert!(stdout.contains("sim/BTCUSDT"), "stdout missing per-engine line: {stdout}");
}

/// NEGATIVE: a journal whose LAST Snap carries a corrupted hash fails the determinism fence
/// inside `replay_offline` — the CLI must surface that as exit 1 + a `REPLAY FAILED` stderr line
/// (doctoring pattern duplicated from `replay_fence.rs::corrupted_final_hash_fails_the_fence`).
#[test]
fn replay_journal_exits_nonzero_on_corrupted_journal() {
    let good = unique_dir("bad-src");
    build_source_journal(&good);
    let bad = unique_dir("bad-dst");
    copy_journal_files(&good, &bad);

    let recs = CommandJournal::read_all(&good).unwrap();
    let (engines, session, seq, good_hash) = recs
        .iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snap { engines, coid_session, coid_seq, hash, .. } => {
                Some((engines.clone(), coid_session.clone(), *coid_seq, *hash))
            }
            _ => None,
        })
        .unwrap();
    let wrong_hash = good_hash.wrapping_add(1);
    {
        let mut j = CommandJournal::open(
            &bad,
            JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
        )
        .unwrap();
        j.append_snap(999, &engines, &session, seq, 0, &[], &[], &[], wrong_hash).unwrap();
        j.flush().unwrap();
    }

    let out = OsCommand::new(REPLAY_JOURNAL).arg(&bad).output().expect("spawn replay_journal");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit 1, got {:?}; stderr: {stderr}",
        out.status
    );
    assert!(out.stdout.is_empty(), "Err path must not print the success summary to stdout");
    assert!(stderr.contains("REPLAY FAILED"), "stderr missing failure line: {stderr}");
    assert!(stderr.contains("HashMismatch"), "stderr should surface the fence variant: {stderr}");
}

/// NEGATIVE (Io path): a nonexistent journal dir also fails — exercises `ReplayError::Io`/`Empty`
/// rather than `HashMismatch`, still surfacing as exit 1 + `REPLAY FAILED`.
#[test]
fn replay_journal_exits_nonzero_on_missing_dir() {
    let missing = unique_dir("missing"); // never created
    let out = OsCommand::new(REPLAY_JOURNAL).arg(&missing).output().expect("spawn replay_journal");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit 1, got {:?}; stderr: {stderr}",
        out.status
    );
    assert!(stderr.contains("REPLAY FAILED"), "stderr missing failure line: {stderr}");
}

/// Usage error: no journal-dir argv → exit 2, nothing on stdout.
#[test]
fn replay_journal_usage_error_without_argv() {
    let out = OsCommand::new(REPLAY_JOURNAL).output().expect("spawn replay_journal");
    assert_eq!(out.status.code(), Some(2), "expected exit 2, got {:?}", out.status);
    assert!(out.stdout.is_empty(), "usage error must not print to stdout");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("usage"), "stderr missing usage line: {stderr}");
}
