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
//! *submitted* from *settled*, and only [`crate::exec_plane::settlement::redeem_confirm`]'s on-chain verdict may write the
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

    /// **The permanent tombstone.** Only [`crate::exec_plane::settlement::redeem_confirm::RedeemConfirmation::Settled`] —
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

#[path = "redeem_ledger_tests.rs"]
#[cfg(test)]
mod redeem_ledger_tests;
