//! **An account that joins LATE gets the book the boot would have restored for it** (owner 2026-11-15, "1.a", and the
//! rule that a refactor never changes working behaviour). A box whose account's venue was not connected at the boot
//! (`vike_model::ArmingBlock::venue_not_connected`: an IB Gateway that was down) starts without that account's engine
//! (`spawn_core_without_accounts` when it was the only one), and the account joins later through
//! `AccountChange::AddEngine`. What the boot restore would have put back on its engine is kept until then:
//!
//! * its journaled ENGINE (positions, PnL, fees, balances, fill dedup, admitted symbols, the trading state): held in
//!   [`AwaitingRestore::engines`], written into every `Snapshot` (`Snapshot.awaiting_engines`, beside `engines` and never
//!   in it: the refold builds its replay core from `engines`, where an account that does not run must route no record),
//!   and put on the account's engine when it joins by the boot's own steps: the composition root's
//!   ([`open_restored_engine_steps`], the tradehub's two snapshot steps), then `boot_restore.rs`'s `restore_onto`;
//! * its ARMED CONDITIONALS (an operator's core-held reduce-only stop included): held OUT of the books (a fire would reach
//!   no engine of the account), written into every `Snapshot`'s `order_emulation_arms`, and re-armed when it joins by the
//!   boot's own steps (`restore_conditional_books_into`, `restore_arm_routes`: a core-held stop is HELD until the
//!   account's first reconcile pass); the mount that armed one is bound to it when that mount mounts
//!   ([`CoreThread::bind_awaiting_arm_owners`]), since a deactivated mount comes right behind its account.
//!
//! The rest of the boot restore needs nothing here: the restored ORDERS wait for the account's own reconcile pass
//! (`restore.rs`: an entry of another account stays for a later pass), a deactivated mount's ledger and orders wait for
//! its mount (`pending_owners`; a join that refused its book for its tier drops the ledger and closed trades instead,
//! as the boot does: [`CoreThread::forget_tier_refused_join_ledger`]), and the contingency book, the held releases and the emulated parents ride every
//! `Snapshot` as they are.
//!
//! Only an account the core is told WAITS for its venue (a `CoreConfig::not_running` row whose block is not-connected)
//! is held; every other journaled engine the core runs no engine for is skipped, loudly, as it always was. A replay core
//! is told no such row, so it holds nothing. Cold: the assembly, a `Snapshot` and an account's join.

use super::*;

use super::boot_restore::{
    RestoreSavedBooks, RestoredOnto, restore_onto, say_skipped, skip_snapshot,
};
use super::core_thread::RestoreSteps;
use vike_execution::EngineSnapshot;
use vike_journal::JournalSnapshotOrderEmulationArm;
use vike_model::accounts::account_keys::venue_of_route_key;

/// The books of the accounts that wait for their venue (module doc).
#[derive(Default)]
pub(crate) struct AwaitingRestore {
    /// Their journaled engines, one per account.
    pub(crate) engines: Vec<EngineSnapshot>,
    /// Their armed conditionals, held out of the books.
    pub(crate) arms: Vec<JournalSnapshotOrderEmulationArm>,
    /// `(arm id, mount id)` of every held arm a mount armed, kept until that mount mounts.
    pub(crate) arm_owners: Vec<(String, String)>,
    /// The engine indices whose late join REFUSED its book for its tier: the mounts that come up on one take
    /// none of the other tier's ledger ([`CoreThread::forget_tier_refused_join_ledger`]).
    pub(crate) tier_refused_engines: Vec<usize>,
}

/// The account a journaled engine is: its route key, else its venue (a default account's snapshot names none).
fn engine_key(snap: &EngineSnapshot) -> &str {
    snap.route_key.as_deref().unwrap_or(&snap.venue)
}

/// Whether a journaled arm is an account's of `waiting`: its route key names one, or it names none (a core with one
/// engine per venue writes none) and its venue is a waiting account's while no engine of that venue runs.
fn arm_waits(
    arm: &JournalSnapshotOrderEmulationArm,
    waiting: &[String],
    engine_venues: &[&str],
) -> bool {
    match arm.route_key.as_deref() {
        Some(key) => waiting.iter().any(|w| w == key),
        None => {
            !engine_venues.contains(&arm.terms.venue.as_str())
                && waiting.iter().any(|w| venue_of_route_key(w) == arm.terms.venue)
        }
    }
}

/// Whether a journaled arm belongs to the account that joins as `route_key` on `venue` (the same rule as [`arm_waits`]).
fn arm_of(arm: &JournalSnapshotOrderEmulationArm, route_key: &str, venue: &str) -> bool {
    match arm.route_key.as_deref() {
        Some(key) => key == route_key,
        None => arm.terms.venue == venue,
    }
}

/// **Open the composition root's steps over journaled engines** (`CoreConfig::restored_engine_steps`) as a function
/// over this core's engines: by `spawn_core_multi`, the one place `C: 'static` is known (as the engine door is). It
/// runs them over the boot's books before the assembly, and the core keeps them for a late join
/// ([`CoreThread::restore_joining_engine`]): ONE function at both. A function over another client type is refused
/// with a note, and the books are restored without it.
pub(crate) fn open_restored_engine_steps<C: ExecutionClient + 'static>(
    steps: Option<Box<dyn std::any::Any + Send>>,
    notes: &mut Vec<String>,
) -> Option<RestoreSteps<C>> {
    let opened = steps?.downcast_ref::<RestoreSteps<C>>().copied();
    if opened.is_none() {
        let note = "RESTORE STEPS REFUSED: the composition root's restore steps are not a function over this core's \
                    engines, so the journaled books are restored without them"
            .to_string();
        tracing::error!(target: "vike_core::core", "{note}");
        notes.push(note);
    }
    opened
}

/// **Take the books of the accounts that wait for their venue out of the boot restore's inputs** (`assemble_core`, before
/// it restores engines and books): the route keys of `config.not_running` whose block is not-connected and that no engine
/// of `engine_keys` carries, their journaled engines out of `config.restored_engines`, their arms out of
/// `config.order_emulation_arms` (with their owners copied from `config.conditional_owners`). One note per held book.
pub(super) fn take_awaiting(
    config: &mut CoreConfig,
    engine_keys: &[&str],
    engine_venues: &[&str],
    notes: &mut Vec<String>,
) -> AwaitingRestore {
    let waiting: Vec<String> = config
        .not_running
        .iter()
        .filter(|nr| nr.block.is_some_and(vike_model::ArmingBlock::venue_not_connected))
        .map(vike_execution::ArmingNotRunningView::route_key)
        .filter(|key| !engine_keys.contains(&key.as_str()))
        .collect();
    if waiting.is_empty() {
        return AwaitingRestore::default();
    }
    let (engines, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut config.restored_engines)
        .into_iter()
        .partition(|s| waiting.iter().any(|w| w == engine_key(s)));
    config.restored_engines = kept;
    let (arms, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut config.order_emulation_arms)
        .into_iter()
        .partition(|a| arm_waits(a, &waiting, engine_venues));
    config.order_emulation_arms = kept;
    let arm_owners: Vec<(String, String)> = config
        .conditional_owners
        .iter()
        .filter(|(arm, _)| arms.iter().any(|a| a.arm_id == *arm))
        .cloned()
        .collect();
    for snap in &engines {
        let key = engine_key(snap);
        let held = arms.iter().filter(|a| arm_of(a, key, &snap.venue)).count();
        let note = format!(
            "RESTORE ENGINE AWAITS ITS ACCOUNT {key}: its venue is not connected, so its journaled book ({} \
             position(s), balance {}, realized PnL {}) and {held} armed conditional(s) are kept and restored when \
             it joins",
            snap.account.positions.len(),
            snap.account.balance,
            snap.account.realized_pnl,
        );
        tracing::warn!(target: "vike_core::core", "{note}");
        notes.push(note);
    }
    AwaitingRestore { engines, arms, arm_owners, tier_refused_engines: Vec::new() }
}

impl<C: ExecutionClient> CoreThread<C> {
    /// **Put the book the boot held for this account on its engine as it joins** (before the engine is appended): the
    /// boot's own rule (`restore_onto`). Returns whether the book was another tier's (refused, as the boot would have,
    /// saving it beside the journal), so the account's held arms are refused too ([`Self::rearm_joining`]).
    pub(super) fn restore_joining_engine(&mut self, engine: &mut Engine<C>) -> bool {
        let Some(at) =
            self.awaiting.engines.iter().position(|s| engine_key(s) == engine.route_key.as_str())
        else {
            return false;
        };
        let mut snap = self.awaiting.engines.remove(at);
        let key = engine.route_key.clone();
        let mut notes: Vec<String> = Vec::new();
        // The steps the boot runs over a booted engine's book, here over this one (one function).
        if let Some(run) = self.restore_steps {
            run(std::slice::from_mut(&mut snap), &[&*engine]);
        }
        let saved = RestoreSavedBooks {
            state_dir: self.config.state_dir.as_deref(),
            snap_sequence: self.config.restored_snap_sequence,
        };
        let mut tier_refused = false;
        match restore_onto(engine, &snap, &key, &saved, &mut notes) {
            RestoredOnto::Applied => {
                let note = format!(
                    "RESTORE ON JOIN {key}: the journaled book the boot held for this account is put back on its \
                     engine ({} position(s), balance {}, realized PnL {}), as the boot would have",
                    snap.account.positions.len(),
                    snap.account.balance,
                    snap.account.realized_pnl,
                );
                tracing::warn!(target: "vike_core::core", "{note}");
                notes.push(note);
            }
            RestoredOnto::TierRefused => tier_refused = true,
            RestoredOnto::Skipped(why) => {
                let mut skipped = Vec::new();
                skip_snapshot(&mut skipped, &key, &snap, Some(&mut *engine), why, &mut notes);
                say_skipped(skipped, &mut notes);
            }
        }
        for n in notes {
            self.note(n);
        }
        tier_refused
    }

    /// **Re-arm the conditionals the boot held for the account that just joined at `index`** (`route_key` on `venue`):
    /// into the books by the boot's own steps, each routed to that engine by its key. A book refused for its tier
    /// (`tier_refused`) refuses its arms too, as the boot does.
    pub(super) fn rearm_joining(
        &mut self,
        index: usize,
        route_key: &str,
        venue: &str,
        tier_refused: bool,
    ) {
        if tier_refused {
            self.awaiting.tier_refused_engines.push(index);
        }
        let (mut arms, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.awaiting.arms)
            .into_iter()
            .partition(|a| arm_of(a, route_key, venue));
        self.awaiting.arms = kept;
        if arms.is_empty() {
            return;
        }
        let mut routes = super::assemble::restore_conditional_books_into(
            &mut self.emulated.order_emulation_arms,
            &mut arms,
        );
        // The account is known here: an arm that named none fires onto the engine that just joined.
        for route in &mut routes {
            route.route_key.get_or_insert_with(|| route_key.to_string());
        }
        let owners = self.awaiting.arm_owners.clone();
        let joined = [index];
        let refused: &[usize] = if tier_refused { &joined } else { &[] };
        self.restore_arm_routes(routes, owners, refused);
    }

    /// **A mount on an engine whose late join refused its book for its tier takes none of that book back**
    /// (the boot's rule, `assemble_core`'s tier-refused block, `forget_refused_mount_ledgers`): the ledger and the
    /// closed trades the restart held for mount id `mid` were slices of the other tier's book, and beside the
    /// joined engine's empty one they would make the mount hold a position the account does not. Called by
    /// `install_mount` for the slot it just grew, before it binds what the restart held for `mid`.
    pub(crate) fn forget_tier_refused_join_ledger(
        &mut self,
        mid: &str,
        slot: usize,
        engine: usize,
    ) {
        if !self.awaiting.tier_refused_engines.contains(&engine) {
            return;
        }
        self.pending_closed.remove(mid);
        let Some(ledger) = self.pending_owners.get_mut(mid).and_then(|p| p.ledger.take()) else {
            return;
        };
        self.mount_attribution[slot] = ledger;
        let mut notes = Vec::new();
        let now = self.engine.now_ms;
        forget_refused_mount_ledgers(
            &[slot],
            &self.mount_ids,
            &mut self.mount_attribution,
            self.order_owners.as_mut(),
            now,
            &mut notes,
        );
        for n in notes {
            self.note(n);
        }
    }

    /// **Bind the arms the account re-armed at its join to the mount that armed them**, once that mount mounts at `slot`
    /// (a deactivated mount comes right behind its account): what the boot does for a mount that was mounted.
    pub(crate) fn bind_awaiting_arm_owners(&mut self, mount_id: &str, slot: usize) {
        if self.awaiting.arm_owners.is_empty() {
            return;
        }
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.awaiting.arm_owners)
            .into_iter()
            .partition(|(_, mid)| mid == mount_id);
        self.awaiting.arm_owners = rest;
        for (arm, _) in mine {
            match self.emulated.cond_engine.get_mut(&arm) {
                Some(route) if route.mount.is_none() => route.mount = Some(slot),
                // Not re-armed yet (its account still waits): keep it for that mount.
                None => self.awaiting.arm_owners.push((arm, mount_id.to_string())),
                Some(_) => {}
            }
        }
    }
}
