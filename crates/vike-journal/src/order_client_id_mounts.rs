//! The PURE order client id -> mount fold behind attribution durability (gap D): which mount MINTED each
//! order, read back out of the journal's existing provenance records. ONE copy, folded one record at a time
//! ([`JournalOrderClientIdMountFold::fold_record`]); its reader is vike-core's restore and refold
//! (`crates/vike-core/src/replay/fold.rs`'s `fold_order_client_id_origins_with_fires`). The datahub folds
//! nothing: each execution report the OMS journals carries its own mount (decision 0295).
//!
//! Nothing new is written to make this work. `apply_strategy_intent` already journals every mounted
//! strategy's intent as [`JournalRecord::StrategySubmit`] carrying its `mount_id`, write-ahead of the
//! `apply_intent` that mints the order client id — and that mint writes its own [`JournalRecord::MintedSubmit`]
//! with the RESOLVED request immediately after, before any other record can be appended (the fold is
//! single-writer and single-threaded). So the pairing rule is positional and exact:
//!
//! - a `StrategySubmit` OPENS ownership by its `mount_id` (and, for the rare intent that already
//!   carries a client-supplied order client id, maps that order client id directly — no `MintedSubmit` follows one);
//! - every following `MintedSubmit` maps its minted order client id to the open owner (a `Bracket` intent mints
//!   three legs and writes three records — all three belong to the mount that armed them); a `MintedSubmit`
//!   that `replaces` an order (a cancel-then-new amend) takes its predecessor's owner, whatever record it follows;
//! - a record that begins a NON-strategy write closes ownership: a `Command` (an operator/GUI command),
//!   an UNOWNED `MarginCallLiquidate` (the account-wide margin-call sweep), a `Snapshot`, or a
//!   `OrderEmulationFire` whose arm no mount armed (an operator's). Their `MintedSubmit`s are therefore
//!   correctly left UNATTRIBUTED.
//! - a `OrderEmulationFire` of an arm that a mount ARMED (a `OrderEmulationArmed` journaled while that
//!   mount's `StrategySubmit` was open) OPENS ownership by the arming mount: the stop is the strategy's
//!   own order, and `CoreThread::submit_fired` attributes it live. A disarm forgets the arm.
//! - a `MarginCallLiquidate` that DOES name a `mount_id` (journal v14 — the per-mount budget latch's
//!   flatten, `CoreThread::latch_mount`) OPENS ownership by that id, exactly like a `StrategySubmit`:
//!   that order is minted by, and closes the attributed position of, one specific mount.
//!
//! The `mount_id` arm closes what used to be a documented residual. The budget latch's flatten
//! attributed its order client id in memory but journaled it indistinguishably from an account-wide
//! liquidation, so on restore that one order came back unattributed and its fill landed in the
//! RESIDUAL row instead of the mount's ledger — under-reporting exactly the realized loss the budget
//! latch exists to bound. A PRE-v14 journal's latch flatten still restores unattributed (the field
//! reads back `None`), which is that journal's own recorded truth rather than a new loss.
//!
//! A reader that starts part-way (a fold resumed past a cursor, a journal whose early
//! segments were pruned) begins with ownership CLOSED, so an order whose opening record it never saw is
//! unattributed: "not known", never a guess.

use std::collections::HashMap;

use crate::JournalRecord;

/// The incremental order client id -> mount fold (the module doc's rule). Fold every record in journal
/// order through [`Self::fold_record`]; `Clone` so a reader that must not fold a failed pass twice folds a
/// copy and keeps it only when the pass lands.
#[derive(Clone, Debug, Default)]
pub struct JournalOrderClientIdMountFold {
    /// order client id -> the mount that minted it. A later mapping of the same id replaces the earlier
    /// one: an amend's replacement takes the LAST owner its predecessor was given.
    mounts: HashMap<String, String>,
    /// The mount ownership is OPEN for (a following `MintedSubmit` is its order); `None` while closed.
    owner: Option<String>,
    /// arm id -> the mount that armed it, for an arm journaled while a mount's ownership was open: the
    /// fire of that arm is THAT mount's own order.
    arm_owners: HashMap<String, String>,
    /// The last arm that fired with an owner, and that owner.
    fired: Option<(String, String)>,
}

impl JournalOrderClientIdMountFold {
    /// Fold ONE record. Returns the `(order client id, mount id)` this record attributes: a `StrategySubmit`'s
    /// client-supplied order client id, or a `MintedSubmit`'s minted one under an open (or inherited) owner.
    pub fn fold_record(&mut self, record: &JournalRecord) -> Option<(String, String)> {
        let attributed = match record {
            JournalRecord::StrategySubmit { mount_id, intent, .. } => {
                self.owner = Some(mount_id.clone());
                // A client-supplied (non-empty) order client id never reaches the minting path, so it has no
                // `MintedSubmit` to pair with — map it here or lose it.
                client_supplied_order_client_id(intent).map(|id| (id, mount_id.clone()))
            }
            JournalRecord::MintedSubmit { request, replaces, .. } => {
                // an amend's replacement is its predecessor's, whatever record the mint follows
                let mount = match replaces {
                    Some(old) => self.mounts.get(old).cloned(),
                    None => self.owner.clone(),
                };
                mount
                    .filter(|_| !request.order_client_id.is_empty())
                    .map(|m| (request.order_client_id.clone(), m))
            }
            // The per-mount budget latch's flatten NAMES its owner (v14): it OPENS ownership, so the
            // `MintedSubmit` that follows — the order client id `apply_intent` mints for it — books into that
            // mount's ledger, identically to the live runtime's own `order_client_id_mount` insert.
            JournalRecord::MarginCallLiquidate { mount_id: Some(mount_id), .. } => {
                self.owner = Some(mount_id.clone());
                None
            }
            // A conditional's FIRE reopens ownership with the mount that ARMED it, so the
            // `MintedSubmit` the release writes restores into that mount's ledger, identically to
            // the live runtime's `submit_fired` write. An arm nobody owned (an operator's, under a
            // `Command`; or one armed below the `Snapshot` the fold started at whose row names no mount, a
            // `Snapshot` written before decision 0304) has no entry and the release CLOSES ownership:
            // unattributed, as before.
            JournalRecord::OrderEmulationArmed { arm_id, .. } => {
                if let Some(mount) = &self.owner {
                    self.arm_owners.insert(arm_id.clone(), mount.clone());
                }
                None
            }
            JournalRecord::OrderEmulationDisarmed { arm_id, .. } => {
                self.arm_owners.remove(arm_id);
                None
            }
            // A stop larger than its position fires as two records of one arm (`submit_fired`): the second is its too.
            JournalRecord::OrderEmulationFire { arm_id, .. } => {
                self.owner = match self.arm_owners.remove(arm_id) {
                    Some(mount) => {
                        self.fired = Some((arm_id.clone(), mount.clone()));
                        Some(mount)
                    }
                    None => {
                        self.fired.as_ref().filter(|(arm, _)| arm == arm_id).map(|(_, m)| m.clone())
                    }
                };
                None
            }
            // A `Snapshot` closes ownership like any non-strategy record, and it names the mount of each arm it holds
            // (`JournalSnapshotOrderEmulationArm::mount_id`, decision 0304): a fold that STARTS at it (the boot
            // restore's read) knows who armed its resting stops without the `OrderEmulationArmed` records below it.
            JournalRecord::Snapshot { order_emulation_arms, .. } => {
                self.owner = None;
                for arm in order_emulation_arms {
                    if let Some(mount) = &arm.mount_id {
                        self.arm_owners.entry(arm.arm_id.clone()).or_insert_with(|| mount.clone());
                    }
                }
                None
            }
            JournalRecord::Command { .. }
            // ...whereas an UNOWNED `MarginCallLiquidate` is the account-wide margin-call sweep
            // (and every pre-v14 frame, which defaults to `None`) — a release no mount originated,
            // so it CLOSES.
            | JournalRecord::MarginCallLiquidate { mount_id: None, .. } => {
                self.owner = None;
                None
            }

            // ── EXHAUSTIVE by design: NO `_` arm (see [`JournalRecord`]'s "Adding a variant"
            // contract). These are observations/markers that MINT NOTHING, so they neither
            // open nor close ownership — an interleaved one must leave a `StrategySubmit` ->
            // `MintedSubmit` pair intact, which is why they are ignored rather than closing:
            //   `PortfolioSnapshot`   — a periodic portfolio observation.
            //   `GtdExpire`           — an expiry DECISION; issues a cancel, mints no order client id.
            //   `ScheduleFire`        — an on_schedule DECISION; the orders it produces arrive as
            //                           their own `StrategySubmit` records, which open ownership.
            //   `SharedCloseTimeout`  — a shared close's DECISION marker; the step's orders arrive
            //                           as their own `StrategySubmit` records.
            //   `StrategyFileMounted` — a mount opened on a strategy file; an observation, mints nothing.
            //   `Forecast`            — a forecast a mount published (W35); mints nothing.
            //   `ExecutionReport`, `ExecutionReportsUnverified` — what the OMS stated after a fold, derived
            //                           from records folded above; mints nothing.
            //   `PositionSnapshot`    — the accounts' positions at a boot, an observation; mints nothing.
            //   `DeniedRepeat`        — coalesced DENIALS: their ids were minted, but a denied order never
            //                           rests or fills, so it owes no owner.
            // A new variant that DOES mint an order client id must be classified into one of the arms above,
            // not added here — the compiler will demand the decision.
            JournalRecord::PortfolioSnapshot { .. }
            | JournalRecord::GtdExpire { .. }
            | JournalRecord::ScheduleFire { .. }
            | JournalRecord::SharedCloseTimeout { .. }
            | JournalRecord::StrategyFileMounted { .. }
            | JournalRecord::Forecast { .. }
            | JournalRecord::ExecutionReport { .. }
            | JournalRecord::ExecutionReportsUnverified { .. }
            | JournalRecord::PositionSnapshot { .. }
            | JournalRecord::DeniedRepeat { .. } => None,
        };
        if let Some((order_client_id, mount)) = &attributed {
            self.mounts.insert(order_client_id.clone(), mount.clone());
        }
        attributed
    }

    /// The mount ownership is open for after the last folded record; right after an `OrderEmulationFire`,
    /// the mount that fire belongs to.
    pub fn open_owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }

    /// The arms a MOUNT still owns at the end of the fold: `arm_id -> mount_id` for every
    /// `OrderEmulationArmed` folded under an open owner and not fired or disarmed since.
    pub fn into_arm_owners(self) -> HashMap<String, String> {
        self.arm_owners
    }
}

/// The order client id a journaled intent ALREADY carries, if any — the one case the fold cannot pair
/// with a following `MintedSubmit`, because a non-empty `order_client_id` skips the mint entirely (and
/// therefore writes no such record).
fn client_supplied_order_client_id(intent: &vike_execution::OrderIntent) -> Option<String> {
    match intent {
        vike_execution::OrderIntent::Submit(req) if !req.order_client_id.is_empty() => {
            Some(req.order_client_id.clone())
        }
        vike_execution::OrderIntent::Submit(_)
        | vike_execution::OrderIntent::SubmitBatch(_)
        | vike_execution::OrderIntent::Cancel(_)
        | vike_execution::OrderIntent::CancelBatch(_)
        | vike_execution::OrderIntent::Modify { .. }
        | vike_execution::OrderIntent::Confirm(_)
        | vike_execution::OrderIntent::MassCancel { .. }
        | vike_execution::OrderIntent::Flatten { .. }
        | vike_execution::OrderIntent::MarketExit { .. }
        | vike_execution::OrderIntent::Bracket(_)
        | vike_execution::OrderIntent::ArmConditional(_)
        | vike_execution::OrderIntent::DisarmConditional { .. }
        | vike_execution::OrderIntent::Combo(_)
        | vike_execution::OrderIntent::OrderList(_)
        | vike_execution::OrderIntent::Exercise(_) => None,
    }
}
