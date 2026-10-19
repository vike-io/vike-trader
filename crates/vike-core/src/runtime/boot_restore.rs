//! The ASSEMBLY half of the boot restore (decision 0127): what `assemble_core` does, once, with the
//! journal's answer (`crate::replay::restore_for_boot`, handed in through `CoreConfig`) — never on
//! the fold.
//!
//! - [`merge_journal_owners`]: the ownership FILE decides who owns an order; the journal only adds
//!   the coids the file does not know (and the facts it lacks), and a disagreement is noted.
//! - [`apply_restored_engines`]: each journaled engine's account state goes back onto the live
//!   engine with the SAME route key, or onto none.
//! - [`CoreThread::restore_arm_routes`]: a restored arm gets its engine and its owning mount back.
//! - [`CoreThread::tell_lost_held_exits`]: a held exit a broken journal lost is a terminal reject
//!   its mount hears, so it does not vanish in silence.
//! - [`CoreThread::restored_paper_orders_gone_at_boot`]: a restored order on a PAPER book is gone —
//!   that book lived in the dead process's memory.
//! - [`CoreThread::note_gated_restores`]: one note per venue whose restored orders will wait,
//!   because `CoreConfig::restore_venue_gate` keeps that venue's adoption off (the table, or a
//!   refused reconcile: no pass will come).
//!
//! Every step is bounded by what the restore handed in (restored orders, arms, engines) and runs
//! before the first message is folded, so none of it is on the latency-gated path.

use super::*;
use crate::order_owners::{OrderOwnerLog, OwnedOrder, OwnerRecord};
use crate::replay::{
    JournalOrder, LostHeldExit, RESTORE_LOST_REASON, RESTORE_PAPER_BOOK_LOST_REASON,
};
use vike_exec::EngineSnapshot;
use vike_model::OrderEventKind;
use vike_model::accounts::account_keys::{AccountLabel, route_key_of};

/// One restored arm's routing facts, as `assemble_core`'s book re-seed read them off the `Snap`.
pub(super) struct ArmRoute {
    pub(super) arm_id: String,
    /// The venue of the book the arm sits in.
    pub(super) venue: String,
    /// `SnapConditional::route_key` (journal v17).
    pub(super) route_key: Option<String>,
}

/// How many coids a restore note lists (the boot notes here and the pass note in `restore.rs`);
/// the rest are counted.
const NOTE_SAMPLE: usize = 8;

/// `items` joined for a note: the first [`NOTE_SAMPLE`], then `(N more)`.
pub(super) fn sample(items: &[String]) -> String {
    let shown: Vec<&str> = items.iter().take(NOTE_SAMPLE).map(String::as_str).collect();
    let more = items.len().saturating_sub(NOTE_SAMPLE);
    if more > 0 { format!("{} ({more} more)", shown.join(", ")) } else { shown.join(", ") }
}

/// Push a boot note and log it once at WARN — a restore fact an operator must see in the trace
/// as well as on the ring.
fn warn_note(notes: &mut Vec<String>, note: String) {
    tracing::warn!(target: "vike_core::core", "{note}");
    notes.push(note);
}

/// THE OWNERSHIP PRECEDENCE (decision 0127, amending 0113's verdict 4): the ownership file's
/// entries (`file`, as loaded) are the answer; each journal candidate either SUPPLEMENTS the file's
/// entry for its coid — the facts the file did not record (venue, symbol and account when the line
/// carried none; the price, which no line carries) — or, when the file does not know the coid at
/// all, becomes an entry of its own, recorded into the file (`OwnerRecord::Own`) so the next boot
/// knows it without the journal. A journal owner that DISAGREES with the file's keeps the file's,
/// and is noted `RESTORE OWNER CONFLICT`. A file that could not be used (`file_usable` false, its
/// error already logged by `OrderOwnerLog::start`) makes every journal candidate journal-only, and
/// that is said once.
pub(super) fn merge_journal_owners(
    mut file: Vec<OwnedOrder>,
    journal: Vec<JournalOrder>,
    mut log: Option<&mut OrderOwnerLog>,
    file_usable: bool,
    now_ms: i64,
    notes: &mut Vec<String>,
) -> Vec<OwnedOrder> {
    if journal.is_empty() {
        return file;
    }
    let index: std::collections::HashMap<String, usize> =
        file.iter().enumerate().map(|(i, o)| (o.coid.clone(), i)).collect();
    let mut conflicts: Vec<String> = Vec::new();
    let mut journal_only: Vec<String> = Vec::new();
    for jo in journal {
        if let Some(&i) = index.get(&jo.coid) {
            let f = &mut file[i];
            if f.mount_id != jo.mount_id {
                conflicts.push(format!(
                    "{} (file `{}`, journal `{}`)",
                    jo.coid, f.mount_id, jo.mount_id
                ));
                continue;
            }
            if f.venue.is_none() && f.symbol.is_none() {
                f.venue = jo.venue;
                f.symbol = jo.symbol;
                f.account = jo.account;
            }
            if f.price.is_none() {
                f.price = jo.price;
            }
            continue;
        }
        if let Some(log) = log.as_deref_mut() {
            log.record(OwnerRecord::Own {
                coid: jo.coid.clone(),
                mount_id: jo.mount_id.clone(),
                ts: now_ms,
                venue: jo.venue.clone(),
                symbol: jo.symbol.clone(),
                account: jo.account.clone(),
                tag: None,
            });
        }
        journal_only.push(jo.coid.clone());
        file.push(OwnedOrder {
            coid: jo.coid,
            mount_id: jo.mount_id,
            venue: jo.venue,
            symbol: jo.symbol,
            account: jo.account,
            tag: None,
            price: jo.price,
        });
    }
    if !conflicts.is_empty() {
        warn_note(
            notes,
            format!(
                "RESTORE OWNER CONFLICT: the ownership file and the journal name different mounts \
                 for {} order(s); the FILE's owner is kept: {}",
                conflicts.len(),
                sample(&conflicts)
            ),
        );
    }
    if !journal_only.is_empty() {
        let lead = match (log.is_some(), file_usable) {
            (true, true) => "RESTORE OWNERS FROM THE JOURNAL: the ownership file did not know",
            (true, false) => "ORDER OWNERS UNREADABLE — ownership comes from the journal only, for",
            (false, _) => "RESTORE OWNERS FROM THE JOURNAL: no ownership file is configured, for",
        };
        warn_note(
            notes,
            format!("{lead} {} resting order(s): {}", journal_only.len(), sample(&journal_only)),
        );
    }
    file
}

/// How strict a trading state is: `Active` < `Reducing` < `Halted`.
fn strictness(state: TradingState) -> u8 {
    match state {
        TradingState::Active => 0,
        TradingState::Reducing => 1,
        TradingState::Halted => 2,
    }
}

/// Put each journaled engine's account state back onto the live engine with its ROUTE KEY
/// (`EngineSnapshot::route_key`, the venue when absent) — `ExecutionEngine::restore_runtime_state`,
/// and the trading state only when it is STRICTER than the live engine's. A snapshot is SKIPPED,
/// with a `RESTORE ENGINE SKIPPED` note, when its key is carried by another snapshot too (two
/// accounts of one venue in a v16 `Snap`: which is which is unknowable), when no engine of this core
/// has its key or its venue, or when it has no route key and its market (symbol, quote asset) is
/// not the live engine's. The note is a WARN, except for a key ending in
/// [`RETIRED_ROUTE_KEY_SUFFIX`] (a stopped engine's tombstone, expected to be skipped): INFO.
pub(super) fn apply_restored_engines<C: ExecutionClient>(
    engine: &mut ExecutionEngine<C>,
    extras: &mut [(f64, ExecutionEngine<C>)],
    snaps: Vec<EngineSnapshot>,
    notes: &mut Vec<String>,
) {
    if snaps.is_empty() {
        return;
    }
    let keys: Vec<String> =
        snaps.iter().map(|s| s.route_key.clone().unwrap_or_else(|| s.venue.clone())).collect();
    let mut applied = 0usize;
    // Each skip keeps its route key beside the note: a tombstone's skip is logged lower (below).
    let mut skipped: Vec<(&String, String)> = Vec::new();
    for (snap, key) in snaps.iter().zip(&keys) {
        let carriers = keys.iter().filter(|k| *k == key).count();
        if carriers > 1 {
            skipped.push((
                key,
                format!(
                    "{key}: {carriers} journaled engines carry that route key (a journal written \
                     before v17 records no account), so none of them is applied"
                ),
            ));
            continue;
        }
        let target = if engine.route_key == *key {
            Some(&mut *engine)
        } else {
            extras.iter_mut().map(|(_, e)| e).find(|e| e.route_key == *key)
        };
        let Some(e) = target else {
            skipped.push((key, format!("{key}: this core runs no engine with that route key")));
            continue;
        };
        if e.venue != snap.venue {
            skipped.push((key, format!("{key}: the journaled engine is venue `{}`", snap.venue)));
            continue;
        }
        if snap.route_key.is_none()
            && (e.symbol != snap.symbol || e.quote_asset != snap.quote_asset)
        {
            skipped.push((
                key,
                format!(
                    "{key}: the journaled engine has no route key and its market {}/{} is not \
                     this engine's {}/{}",
                    snap.symbol, snap.quote_asset, e.symbol, e.quote_asset
                ),
            ));
            continue;
        }
        e.restore_runtime_state(snap);
        if strictness(snap.trading_state) > strictness(e.trading_state) {
            warn_note(
                notes,
                format!(
                    "RESTORE TRADING STATE {key}: {:?} restored from the journal (this boot said \
                     {:?}); only a SetTradingState command loosens it",
                    snap.trading_state, e.trading_state
                ),
            );
            e.trading_state = snap.trading_state;
        }
        applied += 1;
    }
    for (key, s) in skipped {
        let note = format!("RESTORE ENGINE SKIPPED {s}");
        if key.ends_with(RETIRED_ROUTE_KEY_SUFFIX) {
            // A stopped engine's tombstone (`CoreThread::write_snap`) is skipped BY DESIGN: it is on
            // the ring and in the trace, but not a warning.
            tracing::info!(target: "vike_core::core", "{note}");
            notes.push(note);
        } else {
            warn_note(notes, note);
        }
    }
    let summary = format!(
        "journal restore: account state (positions, PnL, fees, balances, fill dedup) put back on \
         {applied} of {} journaled engine(s)",
        snaps.len()
    );
    tracing::info!(target: "vike_core::core", "{summary}");
    notes.push(summary);
}

impl<C: ExecutionClient> CoreThread<C> {
    /// Give each restored arm its `cond_engine` row back: the engine its journaled ROUTE KEY names,
    /// or — for an arm with none — the engine its owning MOUNT trades (`route_of` over the mount's
    /// route, which defers to the venue for a foreign leg); the mount is
    /// `CoreConfig::conditional_owners`'. An arm with neither keeps no row (the payload route, the
    /// declared residual on `CoreConfig::conditionals`).
    pub(super) fn restore_arm_routes(
        &mut self,
        arms: Vec<ArmRoute>,
        owners: Vec<(String, String)>,
    ) {
        let owners: std::collections::HashMap<String, String> = owners.into_iter().collect();
        let mut unrouted: Vec<String> = Vec::new();
        for ArmRoute { arm_id, venue, route_key } in arms {
            let mount =
                owners.get(&arm_id).and_then(|mid| self.mount_ids.iter().position(|id| id == mid));
            let engine = match route_key.as_deref() {
                Some(rk) => self.engine_idx_for_route_key(RouteKey::declared(rk)),
                None => mount.and_then(|slot| self.route_of(EngineRoute::Mount(slot), &venue)),
            };
            match engine {
                Some(engine) => {
                    self.cond_engine.insert(arm_id, ArmedBy { engine, mount });
                }
                None if route_key.is_some() => unrouted.push(arm_id),
                None => {}
            }
        }
        if !unrouted.is_empty() {
            self.note(format!(
                "RESTORE ARMS UNROUTED: {} restored arm(s) name an account this core runs no \
                 engine for; their fire takes the payload route: {}",
                unrouted.len(),
                sample(&unrouted)
            ));
        }
    }

    /// Tell the mount of each HELD exit a broken journal lost (the coid's owner per the ownership
    /// file) with a synthetic terminal `OrderRejected` ([`RESTORE_LOST_REASON`]) through the lane
    /// real lifecycle events use, and forget the entry: nothing will ever hold or release it. A lost
    /// exit whose coid no mount owns is in the PROTECTION LOST line only.
    pub(super) fn tell_lost_held_exits(&mut self, lost: Vec<LostHeldExit>) {
        for exit in lost {
            if self.mount_for_coid(&exit.coid).is_none() {
                continue;
            }
            let idx = self.engine_idx_for_route_key(RouteKey::sole_account_of(&exit.venue));
            let kind = OrderEventKind::Rejected { reason: RESTORE_LOST_REASON.to_string() };
            self.queue_order_event(idx.unwrap_or(0), &exit.venue, &exit.symbol, &exit.coid, kind);
            self.restored_orders.shift_remove(&exit.coid);
            self.record_owner(OwnerRecord::Forget { coid: exit.coid });
        }
    }

    /// Under `CoreConfig::restored_paper_orders_gone`: every restored order (a mounted slot's or a
    /// pending one) whose engine is a PAPER book is GONE at boot — its mount hears `Canceled`
    /// ([`RESTORE_PAPER_BOOK_LOST_REASON`]), the file forgets it, and it is counted as
    /// `restored_gone`. No reconcile pass ever runs for a paper engine, so otherwise the entry (and
    /// the tag refusal it arms) would wait for ever.
    pub(super) fn restored_paper_orders_gone_at_boot(&mut self) {
        if !self.config.restored_paper_orders_gone || self.config.restore_orders_off {
            return;
        }
        let mut gone: Vec<String> = Vec::new();
        let mounted: Vec<(String, String, String, Option<String>, String)> = self
            .restored_orders
            .values()
            .filter_map(|o| {
                Some((
                    o.coid.clone(),
                    o.venue.clone()?,
                    o.symbol.clone()?,
                    o.account.clone(),
                    o.mount_id.clone(),
                ))
            })
            .collect();
        for (coid, venue, symbol, account, mount_id) in mounted {
            let Some(slot) = self.mount_ids.iter().position(|id| *id == mount_id) else {
                continue;
            };
            let Some(idx) = self.restored_engine(slot, &venue, account.as_deref()) else {
                continue;
            };
            if self.eng(idx).tier != vike_model::AccountTier::Paper {
                continue;
            }
            self.restored_orders.shift_remove(&coid);
            self.record_owner(OwnerRecord::Forget { coid: coid.clone() });
            if self.mount_for_coid(&coid).is_some() {
                let kind =
                    OrderEventKind::Canceled { reason: RESTORE_PAPER_BOOK_LOST_REASON.to_string() };
                self.queue_order_event(idx, &venue, &symbol, &coid, kind);
            }
            gone.push(coid);
        }
        // A pending entry (no slot yet) on a paper engine: only forgotten, as a pass would.
        let mut pending_gone: Vec<String> = Vec::new();
        for p in self.pending_owners.values() {
            for o in &p.coids {
                let (Some(venue), Some(_)) = (o.venue.as_deref(), o.symbol.as_deref()) else {
                    continue;
                };
                let key = match o.account.as_deref().map(AccountLabel::parse) {
                    None => venue.to_string(),
                    Some(Ok(label)) => route_key_of(venue, &label),
                    Some(Err(_)) => continue,
                };
                if let Some(idx) = self.engine_idx_for_route_key(RouteKey::declared(&key))
                    && self.eng(idx).tier == vike_model::AccountTier::Paper
                {
                    pending_gone.push(o.coid.clone());
                }
            }
        }
        for coid in &pending_gone {
            self.record_owner(OwnerRecord::Forget { coid: coid.clone() });
        }
        self.pending_owners.retain(|_, p| {
            p.coids.retain(|o| !pending_gone.contains(&o.coid));
            !p.coids.is_empty() || p.ledger.is_some()
        });
        gone.extend(pending_gone);
        if gone.is_empty() {
            return;
        }
        self.restore_counters.restored_gone += gone.len() as u64;
        self.note(format!(
            "RESTORE PAPER BOOK: {} restored order(s) were on a paper book, which a restart \
             empties — reported gone at boot: {}",
            gone.len(),
            sample(&gone)
        ));
    }

    /// One note per VENUE whose restored orders will wait, because `CoreConfig::restore_venue_gate`
    /// keeps that venue's adoption off: the declared table (decision 0120) or, on a box whose
    /// operator refused reconcile, the composition root closing the gate for every venue because no
    /// pass will ever come. Until this note the log was silent about them
    /// (`docs/ops/reconcile-on-restart.md`).
    pub(super) fn note_gated_restores(&mut self) {
        if self.config.restore_orders_off {
            return;
        }
        let mut per_venue: indexmap::IndexMap<String, usize> = indexmap::IndexMap::new();
        let pending = self.pending_owners.values().flat_map(|p| p.coids.iter());
        for o in self.restored_orders.values().chain(pending) {
            if let Some(v) = o.venue.as_deref()
                && !(self.config.restore_venue_gate)(v)
            {
                *per_venue.entry(v.to_string()).or_default() += 1;
            }
        }
        for (venue, n) in per_venue {
            self.note(format!(
                "ORDER RESTORE OFF for {venue}: {n} restored order(s) stay unconfirmed — no pass \
                 will adopt or retire them: this venue's order restore is off in the restore \
                 table, or the operator refused reconcile"
            ));
        }
    }
}
