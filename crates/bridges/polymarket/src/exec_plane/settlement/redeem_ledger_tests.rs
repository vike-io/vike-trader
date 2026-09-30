use super::*;

fn ledger(dir: &tempfile::TempDir) -> RedeemLedger {
    RedeemLedger::open(dir.path().join("redeemed.jsonl"))
}

/// The headline property: a submit (write-ahead + relayer hash) is NOT settled. Only chain
/// proof settles, and only that survives a reopen.
#[test]
fn a_submitted_redemption_is_pending_not_settled() {
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    assert!(!l.is_settled("0xabc"));
    assert_eq!(l.begin("0xabc", 1_000), Some(1));
    assert!(!l.is_settled("0xabc"), "a write-ahead record is not a tombstone");
    l.attach_tx("0xabc", "0xtx");
    assert!(!l.is_settled("0xabc"), "a relayer tx hash is still not a tombstone");
    assert_eq!(
        l.pending("0xabc"),
        Some(PendingRedeem { tx_hash: Some("0xtx".into()), since_ms: 1_000, attempts: 1 })
    );
    l.settle("0xabc", Some("0xtx"), 42);
    assert!(l.is_settled("0xabc"), "chain proof settles");
    assert_eq!(l.pending("0xabc"), None);
}

/// Every state survives a reopen of the file — the property that made the original bug
/// permanent, now carrying the RIGHT state.
#[test]
fn all_three_states_persist_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r.jsonl");
    {
        let l = RedeemLedger::open(path.clone());
        l.begin("0xpending", 7);
        l.attach_tx("0xpending", "0xhash");
        l.begin("0xsettled", 8);
        l.settle("0xsettled", Some("0xdone"), 99);
        l.begin("0xreopened", 9);
        l.reopen("0xreopened");
    }
    let l = RedeemLedger::open(path);
    assert_eq!(
        l.pending("0xpending"),
        Some(PendingRedeem { tx_hash: Some("0xhash".into()), since_ms: 7, attempts: 1 })
    );
    assert!(l.is_settled("0xsettled"));
    assert_eq!(
        l.state("0xsettled"),
        Some(RedeemState::Settled(SettledRedeem { tx_hash: Some("0xdone".into()), block: 99 }))
    );
    assert_eq!(l.state("0xreopened"), None, "a reopened row is eligible again");
}

/// The crash-window record: `begin` alone (no `attach_tx`, no `settle`) must be on disk, and it
/// must come back as PENDING-with-no-tx — never as settled (a forfeit) and never as absent (an
/// immediate re-submit).
#[test]
fn write_ahead_record_survives_a_crash_before_the_relayer_answers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r.jsonl");
    {
        let l = RedeemLedger::open(path.clone());
        l.begin("0xcrash", 5_000);
        // simulate a crash: drop without attach_tx / settle / reopen.
    }
    let l = RedeemLedger::open(path);
    assert!(!l.is_settled("0xcrash"), "must NOT be a tombstone — that would forfeit");
    assert_eq!(
        l.pending("0xcrash"),
        Some(PendingRedeem { tx_hash: None, since_ms: 5_000, attempts: 1 }),
        "must be pending-with-no-tx so the caller's timeout, not a blind retry, decides"
    );
}

#[test]
fn begin_bumps_attempts_and_restarts_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    assert_eq!(l.begin("0x1", 100), Some(1));
    assert_eq!(l.begin("0x1", 500), Some(2));
    let p = l.pending("0x1").unwrap();
    assert_eq!((p.attempts, p.since_ms, p.tx_hash), (2, 500, None));
}

/// Real-money guard: `begin` refuses to downgrade a PROVEN redemption back to in-flight, so a
/// caller that lost its discovery filter still cannot re-submit a settled condition.
#[test]
fn begin_refuses_to_reopen_a_settled_row() {
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    l.settle("0x1", Some("0xa"), 9);
    assert_eq!(l.begin("0x1", 1_000), None, "no write-ahead window on a settled row");
    assert!(l.is_settled("0x1"));
    assert!(l.pending_entries().is_empty());
}

#[test]
fn attach_tx_and_reopen_are_no_ops_on_a_settled_row() {
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    l.begin("0x1", 1);
    l.settle("0x1", Some("0xa"), 7);
    l.attach_tx("0x1", "0xlate");
    l.reopen("0x1");
    assert!(l.is_settled("0x1"), "a proven redemption is never un-proven");
    assert_eq!(
        l.state("0x1"),
        Some(RedeemState::Settled(SettledRedeem { tx_hash: Some("0xa".into()), block: 7 }))
    );
}

#[test]
fn reopen_makes_a_pending_row_eligible_again() {
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    l.begin("0x1", 1);
    l.attach_tx("0x1", "0xdropped");
    l.reopen("0x1");
    assert_eq!(l.state("0x1"), None);
    assert!(l.pending_entries().is_empty());
}

#[test]
fn pending_entries_lists_only_in_flight_rows_in_key_order() {
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    l.begin("0xb", 1);
    l.attach_tx("0xb", "0xtb");
    l.begin("0xa", 2);
    l.begin("0xc", 3);
    l.settle("0xc", None, 1);
    let got: Vec<String> = l.pending_entries().into_iter().map(|(c, _)| c).collect();
    assert_eq!(got, vec!["0xa".to_string(), "0xb".to_string()], "settled rows excluded, sorted");
}

/// Keys are normalised, so the data-api's spelling and the chain's spelling hit one row.
#[test]
fn condition_id_keys_are_case_normalised() {
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    l.begin("0xABCdef", 1);
    assert!(l.pending("0xabcdef").is_some());
    l.settle("0xAbCdEf", None, 3);
    assert!(l.is_settled("0xabcdef"));
    assert_eq!(l.pending_entries().len(), 0);
}

/// The old file format (one bare conditionId per line) still reads as settled — an operator
/// skip list or a pre-fix ledger must not suddenly become eligible.
#[test]
fn legacy_bare_condition_id_lines_read_as_settled() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.txt");
    std::fs::write(&path, "0xold1\n\n0xOLD2\n").unwrap();
    let l = RedeemLedger::open(path);
    assert!(l.is_settled("0xold1"));
    assert!(l.is_settled("0xold2"), "legacy lines are case-normalised too");
    assert!(!l.is_settled("0xnever"));
}

/// A legacy file that later gains new-format records: both read, newest wins per cid.
#[test]
fn mixed_legacy_and_json_records_replay_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mixed.jsonl");
    std::fs::write(
            &path,
            "0xlegacy\n{\"cid\":\"0xa\",\"state\":\"pending\",\"since_ms\":5,\"attempts\":2}\n{\"cid\":\"0xa\",\"state\":\"settled\",\"block\":11}\n{\"cid\":\"0xb\",\"state\":\"pending\",\"since_ms\":6}\n{\"cid\":\"0xb\",\"state\":\"open\"}\n",
        )
        .unwrap();
    let l = RedeemLedger::open(path);
    assert!(l.is_settled("0xlegacy"));
    assert!(l.is_settled("0xa"), "the later settled record wins over the earlier pending one");
    assert_eq!(l.state("0xb"), None, "the later open record wins over the earlier pending one");
}

/// `settle` is idempotent and does not append a second tombstone.
#[test]
fn settle_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r.jsonl");
    let l = RedeemLedger::open(path.clone());
    l.settle("0x1", Some("0xa"), 5);
    l.settle("0x1", Some("0xa"), 5);
    assert!(l.is_settled("0x1"));
    let lines = std::fs::read_to_string(&path).unwrap();
    assert_eq!(lines.lines().filter(|l| l.contains("settled")).count(), 1);
}
