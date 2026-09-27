//! `redeem_ledger` — the crash-safe, THREE-state idempotency store for CTF auto-redeem: a
//! persisted record of what has been submitted and what has been PROVEN on-chain.
//!
//! ## What changed, and why (the bug this file used to have)
//!
//! This ledger was a flat set of conditionIds with one verb, `mark`, called on a **relayer HTTP
//! 2xx**. A 2xx means the gasless relayer accepted a signed batch (`{"state":"NEW"}`, often with no
//! `transactionHash` yet) — it is not evidence that the redemption was mined, let alone that it
//! succeeded. Because the set is persisted, a dropped or reverted transaction wrote a PERMANENT
//! tombstone: the poller's discovery filter skipped that conditionId forever and nothing else in
//! the system retried it, so real winnings were silently forfeited. The ledger now distinguishes
//! *submitted* from *settled*, and only [`crate::redeem_confirm`]'s on-chain verdict may write the
//! permanent one.
//!
//! ## The two errors, and which one this design prefers
//!
//! Any at-most-once guard over an unreliable submit path must choose which way to fail under a
//! crash. This one **prefers a bounded, delayed DUPLICATE SUBMIT over a permanent FORFEIT**, on
//! three grounds:
//!
//! 1. **The real at-most-once guard is on-chain, not in this file.** `redeemPositions` pays out
//!    against the caller's CURRENT conditional-token balance and burns it. Once a redemption has
//!    settled the balance is zero, so a second call pays nothing (CTF) or reverts (the
//!    NegRiskAdapter path, whose explicit per-slot amounts no longer match the balance). A
//!    duplicate submit is a wasted relayer call, NOT a double payout.
//! 2. **The two costs are not comparable.** A forfeit is an unbounded loss of a resolved winning
//!    position. A duplicate submit costs one relayer round trip that the relayer pays gas for.
//! 3. **Discovery is a second gate.** A re-submit only ever happens for a conditionId the data-api
//!    STILL reports as `redeemable` on a later tick. If the first submit landed, the position is
//!    gone from that list and no re-submit is ever attempted — the timeout below fires only in the
//!    case where a retry is the right answer.
//!
//! So: this file never writes `Settled` without chain proof, and it accepts that a crash inside
//! the submit window can cost one extra relayer call.
//!
//! ## The three states
//!
//! * **absent** — never submitted. Eligible.
//! * [`RedeemState::Pending`] — a submit is in flight or unresolved. Written **before** the relayer
//!   is contacted ([`RedeemLedger::begin`], the write-ahead record), so a crash anywhere in the
//!   submit window leaves evidence rather than a silent gap. Suppresses re-submission for a
//!   caller-chosen window; NEVER a tombstone.
//! * [`RedeemState::Settled`] — proven on-chain ([`RedeemLedger::settle`]). The permanent tombstone,
//!   and the only state [`RedeemLedger::is_settled`] reports.
//!
//! ## The file
//!
//! Append-only JSON-lines, last record per conditionId wins — so a state transition is one `write`
//! plus an `fsync`, never a rewrite of the whole file (which could truncate it into nothing under a
//! crash). Three record shapes:
//!
//! ```text
//! {"cid":"0x…","state":"pending","tx":"0x…","since_ms":1730000000000,"attempts":1}
//! {"cid":"0x…","state":"settled","tx":"0x…","block":90715029}
//! {"cid":"0x…","state":"open"}
//! ```
//!
//! **Legacy tolerance:** a non-JSON, non-empty line is read as a bare conditionId in state
//! `settled` — the format the old `mark` wrote, and the shape an operator would hand-write as a
//! "never touch this one" skip list. Honouring it is the safe read of both.
//!
//! Persistence stays **best-effort** (a write failure warns; the in-memory map still guards the
//! session), which is sound precisely because of the preference stated above: a lost write-ahead
//! record can only cause a duplicate submit, never a forfeit.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::Value;

/// A redemption that has been submitted (or is about to be) but is not yet proven on-chain.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingRedeem {
    /// The relayer's transaction hash, when it gave one. `None` is the NORMAL case immediately
    /// after a submit (the relayer answers `{"state":"NEW"}` with no hash) and is also what a crash
    /// inside the submit window leaves behind — the two are deliberately indistinguishable, and
    /// resolve the same way.
    pub tx_hash: Option<String>,
    /// When this pending window opened (unix ms) — the clock the caller's timeout runs against.
    pub since_ms: i64,
    /// How many submits have been made for this conditionId (1 after the first `begin`).
    pub attempts: u32,
}

/// A redemption PROVEN on-chain. The permanent tombstone.
#[derive(Debug, Clone, PartialEq)]
pub struct SettledRedeem {
    pub tx_hash: Option<String>,
    /// The block the redemption was mined in (`0` for a legacy bare-line entry, which carries none).
    pub block: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RedeemState {
    Pending(PendingRedeem),
    Settled(SettledRedeem),
}

/// Persisted redeem state, keyed by conditionId. Thread-safe (the poller drives it from its own
/// thread).
pub struct RedeemLedger {
    path: PathBuf,
    /// `BTreeMap`, not a hash map: [`RedeemLedger::pending_entries`] drives a real-money sweep, and
    /// a deterministic order makes that sweep reproducible.
    entries: Mutex<BTreeMap<String, RedeemState>>,
}

/// Ledger keys are normalised (lower-case, `0x` kept as written) so a conditionId that arrives
/// spelled differently by two sources still hits the same row.
fn key(cid: &str) -> String {
    cid.trim().to_ascii_lowercase()
}

impl RedeemLedger {
    /// Open (creating the parent dir on first write), replaying any existing records.
    pub fn open(path: PathBuf) -> Self {
        let mut entries: BTreeMap<String, RedeemState> = BTreeMap::new();
        if let Ok(txt) = std::fs::read_to_string(&path) {
            for line in txt.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match parse_record(line) {
                    Some((cid, Some(state))) => {
                        entries.insert(cid, state);
                    }
                    // an `open` record: the row goes back to eligible.
                    Some((cid, None)) => {
                        entries.remove(&cid);
                    }
                    None => tracing::warn!(%line, "RedeemLedger: unparseable record skipped"),
                }
            }
        }
        RedeemLedger { path, entries: Mutex::new(entries) }
    }

    /// The state of one conditionId, if any.
    pub fn state(&self, cid: &str) -> Option<RedeemState> {
        self.entries.lock().unwrap().get(&key(cid)).cloned()
    }

    /// **The discovery filter.** `true` ONLY for a redemption proven on-chain — a pending submit is
    /// deliberately not "settled", because the whole point is that a 2xx does not prove anything.
    pub fn is_settled(&self, cid: &str) -> bool {
        matches!(self.state(cid), Some(RedeemState::Settled(_)))
    }

    /// The pending record for one conditionId, if it is in flight.
    pub fn pending(&self, cid: &str) -> Option<PendingRedeem> {
        match self.state(cid) {
            Some(RedeemState::Pending(p)) => Some(p),
            _ => None,
        }
    }

    /// Every in-flight redemption, oldest key first — what the poller's confirmation sweep walks.
    pub fn pending_entries(&self) -> Vec<(String, PendingRedeem)> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(cid, st)| match st {
                RedeemState::Pending(p) => Some((cid.clone(), p.clone())),
                RedeemState::Settled(_) => None,
            })
            .collect()
    }

    /// **The write-ahead record — call this BEFORE the relayer is contacted.** Opens (or re-opens)
    /// the pending window for `cid` and returns the attempt number. A crash between this call and
    /// the submit leaves a `Pending` row with no transaction hash, which is exactly the state a
    /// successful-POST-then-crash leaves too: both resolve through the caller's timeout.
    ///
    /// Re-calling on an existing pending row bumps `attempts` and restarts the window (a re-submit
    /// legitimately restarts the clock).
    ///
    /// Returns `None` — refusing to open the window at all — if `cid` is already SETTLED. A proven
    /// redemption can never be downgraded back to in-flight, so a caller that skipped its discovery
    /// filter still cannot re-submit one. The caller must not contact the relayer on `None`.
    pub fn begin(&self, cid: &str, now_ms: i64) -> Option<u32> {
        let k = key(cid);
        let attempts = {
            let mut g = self.entries.lock().unwrap();
            let attempts = match g.get(&k) {
                Some(RedeemState::Settled(_)) => return None,
                Some(RedeemState::Pending(p)) => p.attempts.saturating_add(1),
                None => 1,
            };
            g.insert(
                k.clone(),
                RedeemState::Pending(PendingRedeem { tx_hash: None, since_ms: now_ms, attempts }),
            );
            attempts
        };
        self.append(serde_json::json!({
            "cid": k, "state": "pending", "since_ms": now_ms, "attempts": attempts,
        }));
        Some(attempts)
    }

    /// Record the relayer's transaction hash against an in-flight redemption. A no-op unless `cid`
    /// is currently pending — a settled row is never re-opened by a late relayer response.
    pub fn attach_tx(&self, cid: &str, tx_hash: &str) {
        let k = key(cid);
        let record = {
            let mut g = self.entries.lock().unwrap();
            match g.get_mut(&k) {
                Some(RedeemState::Pending(p)) => {
                    p.tx_hash = Some(tx_hash.to_string());
                    Some(serde_json::json!({
                        "cid": k, "state": "pending", "tx": tx_hash,
                        "since_ms": p.since_ms, "attempts": p.attempts,
                    }))
                }
                _ => None,
            }
        };
        if let Some(r) = record {
            self.append(r);
        }
    }

    /// **The permanent tombstone.** Only [`crate::redeem_confirm::RedeemConfirmation::Settled`] —
    /// a mined, successful receipt carrying this conditionId's `PayoutRedemption`, buried
    /// `MIN_CONFIRMATIONS` deep — may reach this.
    pub fn settle(&self, cid: &str, tx_hash: Option<&str>, block: u64) {
        let k = key(cid);
        let newly = {
            let mut g = self.entries.lock().unwrap();
            let already = matches!(g.get(&k), Some(RedeemState::Settled(_)));
            g.insert(
                k.clone(),
                RedeemState::Settled(SettledRedeem { tx_hash: tx_hash.map(str::to_string), block }),
            );
            !already
        };
        if !newly {
            return;
        }
        let mut rec = serde_json::json!({ "cid": k, "state": "settled", "block": block });
        if let Some(tx) = tx_hash {
            rec["tx"] = Value::String(tx.to_string());
        }
        self.append(rec);
    }

    /// Drop `cid` back to eligible — the on-chain verdict said the redemption did NOT happen
    /// (reverted, or the transaction was dropped). A no-op on a settled row: a proven redemption is
    /// never un-proven.
    pub fn reopen(&self, cid: &str) {
        let k = key(cid);
        let removed = {
            let mut g = self.entries.lock().unwrap();
            // `matches!` first, so the read borrow ends before `remove` takes the write borrow.
            let in_flight = matches!(g.get(&k), Some(RedeemState::Pending(_)));
            in_flight && g.remove(&k).is_some()
        };
        if removed {
            self.append(serde_json::json!({ "cid": k, "state": "open" }));
        }
    }

    /// Append one record and flush it to the device. Best-effort: a failure warns and the
    /// in-memory map still guards this session (see the module doc for why that is sound here).
    fn append(&self, record: Value) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            Ok(mut f) => {
                if let Err(e) = writeln!(f, "{record}") {
                    tracing::warn!(%record, %e, "RedeemLedger: persist failed (in-memory guard holds)");
                    return;
                }
                // The write-ahead record only helps if it survives the crash it exists for.
                if let Err(e) = f.sync_data() {
                    tracing::warn!(%record, %e, "RedeemLedger: fsync failed (record written, durability not guaranteed)");
                }
            }
            Err(e) => tracing::warn!(%record, %e, "RedeemLedger: open-for-append failed"),
        }
    }
}

/// One persisted line ⇒ `(cid, Some(state))`, `(cid, None)` for an `open` record, or `None` if the
/// line is unusable. A non-JSON line is the LEGACY bare-conditionId format and reads as `settled`.
fn parse_record(line: &str) -> Option<(String, Option<RedeemState>)> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        // legacy: a bare conditionId, written by the pre-fix `mark`, or an operator skip list.
        return Some((
            key(line),
            Some(RedeemState::Settled(SettledRedeem { tx_hash: None, block: 0 })),
        ));
    };
    let Some(cid) = v.get("cid").and_then(Value::as_str) else {
        // JSON, but not one of ours (e.g. a bare quoted string) — fall back to the legacy read of
        // a plain string, otherwise give up on the line.
        return v.as_str().map(|s| {
            (key(s), Some(RedeemState::Settled(SettledRedeem { tx_hash: None, block: 0 })))
        });
    };
    let cid = key(cid);
    let tx = v.get("tx").and_then(Value::as_str).map(str::to_string);
    match v.get("state").and_then(Value::as_str).unwrap_or("settled") {
        "open" => Some((cid, None)),
        "pending" => Some((
            cid,
            Some(RedeemState::Pending(PendingRedeem {
                tx_hash: tx,
                since_ms: v.get("since_ms").and_then(Value::as_i64).unwrap_or(0),
                attempts: v.get("attempts").and_then(Value::as_u64).unwrap_or(1) as u32,
            })),
        )),
        _ => Some((
            cid,
            Some(RedeemState::Settled(SettledRedeem {
                tx_hash: tx,
                block: v.get("block").and_then(Value::as_u64).unwrap_or(0),
            })),
        )),
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(
            got,
            vec!["0xa".to_string(), "0xb".to_string()],
            "settled rows excluded, sorted"
        );
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
}
