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
use crate::order_owners::{OrderOwnerLog, OwnedOrder, OwnerRecord, PROTECTIVE_STOP_MOUNT};
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
    /// `ConditionalRecord::reduce_only`: an operator's core-held reduce-only stop.
    pub(super) reduce_only: bool,
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

/// The operator's trace of ownership that found no mount: one note per mount id, in id order. The
/// pseudo owner of an operator's stop ([`PROTECTIVE_STOP_MOUNT`]) is no mount that will ever land, so
/// its note says what waits for what instead of promising a mount.
pub(super) fn pending_owner_notes(
    pending: &std::collections::HashMap<String, PendingOwner>,
) -> Vec<String> {
    let mut held: Vec<(&String, &PendingOwner)> = pending.iter().collect();
    held.sort_unstable_by(|a, b| a.0.cmp(b.0));
    held.into_iter()
        .map(|(id, p)| {
            if id == PROTECTIVE_STOP_MOUNT {
                let coids: Vec<String> = p.coids.iter().map(|o| o.coid.clone()).collect();
                return format!(
                    "RESTORE OPERATOR STOPS: {} resting protective stop(s) no mount minted wait \
                     for the first reconcile pass to adopt them: {}",
                    coids.len(),
                    sample(&coids)
                );
            }
            format!(
                "ORDER OWNERS HELD: mount `{id}` is not mounted — {} resting order(s){} wait for a \
                 mount of that id; until one lands their fills reach no mount's ledger",
                p.coids.len(),
                if p.ledger.is_some() { " and its ledger" } else { "" }
            )
        })
        .collect()
}

/// THE OWNERSHIP PRECEDENCE (decision 0127, amending 0113's verdict 4): the ownership file's
/// entries (`file`, as loaded) are the answer; each journal candidate either SUPPLEMENTS the file's
/// entry for its coid — the facts the file did not record (venue, symbol and account when the line
/// carried none; the kind and flags a line written before they existed lacks), plus the price and
/// trigger price the JOURNAL knows, which WIN over the line's mint-time ones (a modify moves the
/// order after the line is written; the file's are the fallback) — or, when the file does not know
/// the coid at all, becomes an entry of its own, recorded into the file (`OwnerRecord::Own`) so the
/// next boot knows it without the journal. A journal owner that DISAGREES with the file's keeps the file's,
/// and is noted `RESTORE OWNER CONFLICT`. A file that could not be used (`file_usable` false, its
/// error already logged by `OrderOwnerLog::start`) makes every journal candidate journal-only, and
/// that is said once.
///
/// **An operator's stop is adopted on a `demo` book only.** A candidate under the pseudo mount
/// ([`PROTECTIVE_STOP_MOUNT`]) is a stop no mount minted; adopting it puts it in an engine registry,
/// and a registry-driven cancel (the link dead-man, `enter_safe_state`, a `MarketExit`) then reaches
/// it. Until the owner rules on `live`, the candidate of a book whose snapshot did not state `demo`,
/// or of a venue whose order restore is closed (`restore_venue_gate`: a stop that no pass will ever
/// adopt must not be announced as waiting for one), is not filed at all: it stays what it was, an
/// unknown order at the venue, and one WARN note says how many.
pub(super) fn merge_journal_owners(
    mut file: Vec<OwnedOrder>,
    journal: Vec<JournalOrder>,
    mut log: Option<&mut OrderOwnerLog>,
    file_usable: bool,
    now_ms: i64,
    restore_venue_gate: fn(&str) -> bool,
    notes: &mut Vec<String>,
) -> Vec<OwnedOrder> {
    if journal.is_empty() {
        return file;
    }
    let index: std::collections::HashMap<String, usize> =
        file.iter().enumerate().map(|(i, o)| (o.coid.clone(), i)).collect();
    let mut conflicts: Vec<String> = Vec::new();
    let mut journal_only: Vec<String> = Vec::new();
    let mut declined: Vec<String> = Vec::new();
    for jo in journal {
        if let Some(&i) = index.get(&jo.coid) {
            let f = &mut file[i];
            // The pseudo owner of an operator's protective stop is a fallback GUESS ("no mount
            // minted this"), never a claim: against a real owner in the file it is no conflict, and
            // the facts the journal knows about the order still supplement the entry.
            if f.mount_id != jo.mount_id && jo.mount_id != PROTECTIVE_STOP_MOUNT {
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
            // The journal's terms win where it knows them: the `own` line was written at the mint, and
            // a modify since moved the order (the journal folds the registry's CURRENT request); the
            // file's terms are the fallback for a journal that carries none.
            f.price = jo.price.or(f.price);
            f.trigger_price = jo.trigger_price.or(f.trigger_price);
            f.reduce_only |= jo.reduce_only;
            if f.order_type.is_none() {
                f.order_type = jo.order_type;
            }
            continue;
        }
        if jo.mount_id == PROTECTIVE_STOP_MOUNT
            && !(jo.tier == Some(vike_model::AccountTier::Demo)
                && jo.venue.as_deref().is_some_and(restore_venue_gate))
        {
            declined.push(jo.coid);
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
                order_type: jo.order_type.clone(),
                price: jo.price,
                trigger_price: jo.trigger_price,
                reduce_only: jo.reduce_only,
                venue_id: None,
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
            reduce_only: jo.reduce_only,
            trigger_price: jo.trigger_price,
            order_type: jo.order_type,
            venue_order_id: None,
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
    if !declined.is_empty() {
        warn_note(
            notes,
            format!(
                "RESTORE OPERATOR STOPS NOT ADOPTED: {} resting protective stop(s) no mount minted \
                 stay unknown to the engine (an operator's stop is adopted on a demo book of a venue \
                 whose order restore is open, and nowhere else): {}",
                declined.len(),
                sample(&declined)
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

/// What a snapshot of ANOTHER TIER's book held, for the note [`refuse_other_tier`] writes: the open
/// positions (`SYMBOL +size`), the live resting orders and the money. `positions` is what decides
/// whether the refusal is loud.
struct OtherBook {
    positions: Vec<String>,
    resting: usize,
    balance: f64,
    realized_pnl: f64,
    fees_paid: f64,
    fill_ids: usize,
}

impl OtherBook {
    fn of(snap: &EngineSnapshot) -> Self {
        Self {
            positions: snap
                .account
                .positions
                .iter()
                .filter(|(_, p)| p.size != 0.0)
                .map(|((_, symbol, _), p)| format!("{symbol} {:+} @ {}", p.size, p.avg_px))
                .collect(),
            resting: snap.registry.iter().filter(|(_, o)| !o.status.is_terminal()).count(),
            balance: snap.account.balance,
            realized_pnl: snap.account.realized_pnl,
            fees_paid: snap.account.fees_paid,
            fill_ids: snap.seen_trade_ids.len(),
        }
    }
}

/// **A snapshot that states a tier other than the engine's is ANOTHER BOOK, and none of it is
/// restored** — not the positions, PnL, fees, balances or fill dedup `restore_runtime_state` would
/// put back, and not the trading state. The Binance lane twin `binance#PERP` carries one route key at
/// `demo` and at `live` (decision 0184), and a `paper` engine's key is its `demo` successor's: a cold
/// tier flip (the tradehub down while the account row changes) would otherwise put a simulated or
/// demo position into the real book, where a flatten sizes from it and `quarantine` does not fold
/// the phantom away. (A HOT flip never reaches here: the stopped engine's tombstone is journaled
/// under another key, and the new engine starts empty.)
///
/// A book that was FLAT loses nothing: a WARN says what was left out. One that held a position is
/// LOUD, because the operator changed the tier with something open and must look: an ERROR line
/// naming the key, both tiers and what was left out (with each position's average price and the
/// `Snap` it came from), the same words on the ring (the door, the GUI), and the engine comes up
/// HALTED, so it opens nothing until an operator resumes it (`Command::SetTradingState`). The book
/// itself (positions, resting orders with their ids) is SAVED under the state directory first
/// ([`save_refused_book`]): the journal's next `Snap` replaces it as the restore base, and a flip
/// back to this tier would otherwise find a flat book while the venue keeps the position.
///
/// The journaled TRADING STATE survives the refusal when it is stricter (decision 0127, verdict 7:
/// only a `SetTradingState` command loosens a restored halt): an operator who halted the node and
/// then changed an account's tier finds that account halted too, not trading at the new tier.
fn refuse_other_tier<C: ExecutionClient>(
    e: &mut ExecutionEngine<C>,
    snap: &EngineSnapshot,
    snap_tier: vike_model::AccountTier,
    key: &str,
    saved: &SavedBooks<'_>,
    notes: &mut Vec<String>,
) {
    let book = OtherBook::of(snap);
    let left_out = format!(
        "{} resting order(s), balance {}, realized PnL {}, fees {}, {} fill id(s)",
        book.resting, book.balance, book.realized_pnl, book.fees_paid, book.fill_ids
    );
    let (journaled, running) = (snap_tier.as_str(), e.tier.as_str());
    let seq = saved.snap_seq.map_or_else(|| "unknown".to_string(), |s| s.to_string());
    let kept = if book.positions.is_empty() && book.resting == 0 {
        None
    } else {
        Some(save_refused_book(saved, key, snap, snap_tier, e.tier))
    };
    let kept = match kept {
        None => String::new(),
        Some(Ok(path)) => format!(" The refused book is saved to {}.", path.display()),
        Some(Err(why)) => format!(" The refused book could NOT be saved ({why})."),
    };
    if book.positions.is_empty() {
        warn_note(
            notes,
            format!(
                "RESTORE ENGINE SKIPPED {key}: the journaled engine ran at tier `{journaled}` and \
                 this engine runs at tier `{running}`, so its state was NOT restored; it held no \
                 open position ({left_out}; journal snap seq {seq}).{kept}"
            ),
        );
        carry_trading_state(e, snap.trading_state, key, notes);
        return;
    }
    let note = format!(
        "RESTORE ENGINE SKIPPED {key}: TIER MISMATCH. The journaled engine ran at tier \
         `{journaled}` and this engine runs at tier `{running}`, so none of its state was restored: \
         {} open position(s) [{}], {left_out}; journal snap seq {seq}. Those positions belong to \
         the `{journaled}` account, not to this one.{kept} This engine starts with an EMPTY book \
         and is HALTED until an operator resumes it (SetTradingState; `vike-cli trade account-state \
         <book> active` resumes this one engine and no other, unless every engine of the node came \
         up halted, which reads as a node-wide halt that only `trade resume` lifts)",
        book.positions.len(),
        sample(&book.positions),
    );
    tracing::error!(target: "vike_core::core", "{note}");
    notes.push(note);
    e.trading_state = TradingState::Halted;
}

/// Raise `e` to the journaled trading state `state` when that is STRICTER than the engine's (a
/// restored halt is only ever loosened by a `SetTradingState` command), and say so. The one place the
/// rule is spelled: a restored engine and a tier-refused one both go through it.
fn carry_trading_state<C: ExecutionClient>(
    e: &mut ExecutionEngine<C>,
    state: TradingState,
    key: &str,
    notes: &mut Vec<String>,
) {
    if strictness(state) > strictness(e.trading_state) {
        warn_note(
            notes,
            format!(
                "RESTORE TRADING STATE {key}: {state:?} restored from the journal (this boot said \
                 {:?}); only a SetTradingState command loosens it",
                e.trading_state
            ),
        );
        e.trading_state = state;
    }
}

/// Where a refused book is saved, and the `Snap` it came from.
pub(super) struct SavedBooks<'a> {
    /// `CoreConfig::state_dir`; `None` = nothing is saved (the note says so).
    pub(super) state_dir: Option<&'a std::path::Path>,
    /// `CoreConfig::restored_snap_seq`.
    pub(super) snap_seq: Option<u64>,
}

/// The subdirectory of `CoreConfig::state_dir` a refused book is saved in.
const REFUSED_BOOKS_DIR: &str = "refused-books";

/// **Save the book of a refused snapshot where an operator can find it**:
/// `<state_dir>/refused-books/<route key>-<snap seq>.json` (every character of the key outside
/// `[A-Za-z0-9._-]` becomes `_`, so `binance#PERP` is `binance_PERP`; `noseq` when the restore named
/// no `Snap`). It holds the positions (size and average price), the resting orders with their ids
/// and the money of the journaled engine, so a book that is refused and then replaced as the restore
/// base (the engine of the new tier writes its own `Snap`) can still be recovered by hand.
///
/// Written ONCE per refusal and never removed by code: an existing file is left as it is, which is
/// also what makes a boot that repeats the refusal harmless. A failure is returned as text for the
/// note, never a refused start. The state directory is the daemon's own writable tree, not the
/// settings database or a credential store.
fn save_refused_book(
    saved: &SavedBooks<'_>,
    key: &str,
    snap: &EngineSnapshot,
    snap_tier: vike_model::AccountTier,
    engine_tier: vike_model::AccountTier,
) -> Result<std::path::PathBuf, String> {
    use std::io::Write;
    let Some(root) = saved.state_dir else {
        return Err("no state directory is configured".to_string());
    };
    let dir = root.join(REFUSED_BOOKS_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let safe: String = key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' })
        .collect();
    let seq = saved.snap_seq.map_or_else(|| "noseq".to_string(), |s| s.to_string());
    let path = dir.join(format!("{safe}-{seq}.json"));
    let resting: Vec<_> = snap.registry.iter().filter(|(_, o)| !o.status.is_terminal()).collect();
    let body = serde_json::json!({
        "what": "the journaled book of another tier, refused by the boot restore: nothing of it \
                 was restored into the running engine",
        "route_key": key,
        "journaled_tier": snap_tier.as_str(),
        "engine_tier": engine_tier.as_str(),
        "journal_snap_seq": saved.snap_seq,
        "venue": snap.venue,
        "symbol": snap.symbol,
        "extra_symbols": snap.extra_symbols,
        "trading_state": snap.trading_state,
        "snapshot_now_ms": snap.now_ms,
        "positions": snap.account.positions,
        "resting_orders": resting,
        "balance": snap.account.balance,
        "realized_pnl": snap.account.realized_pnl,
        "fees_paid": snap.account.fees_paid,
    });
    let text = serde_json::to_string_pretty(&body).map_err(|e| e.to_string())?;
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            file.write_all(text.as_bytes())
                .and_then(|()| file.sync_all())
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        // An earlier refusal of this very snapshot saved it already: keep that file.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("{}: {e}", path.display())),
    }
    Ok(path)
}

/// Put each journaled engine's account state back onto the live engine with its ROUTE KEY
/// (`EngineSnapshot::route_key`, the venue when absent) — `ExecutionEngine::restore_runtime_state`,
/// and the trading state only when it is STRICTER than the live engine's. A snapshot is SKIPPED,
/// with a `RESTORE ENGINE SKIPPED` note, when its key is carried by another snapshot too (two
/// accounts of one venue in a v16 `Snap`: which is which is unknowable), when no engine of this core
/// has its key or its venue, when it STATES a tier other than the engine's ([`refuse_other_tier`]:
/// a snapshot that states none is restored as it always was), or when it has no route key and its
/// market (symbol, quote asset) is not the live engine's. The note is a WARN, except for a key
/// ending in [`RETIRED_ROUTE_KEY_SUFFIX`] (a stopped engine's tombstone, expected to be skipped):
/// INFO, and a tier mismatch over an open position: ERROR.
///
/// Returns the INDEX (0 the primary, then the extras in order) of every engine whose snapshot was
/// refused for its tier, so the caller can drop the per-mount ledgers of the mounts that trade it.
pub(super) fn apply_restored_engines<C: ExecutionClient>(
    engine: &mut ExecutionEngine<C>,
    extras: &mut [(f64, ExecutionEngine<C>)],
    snaps: Vec<EngineSnapshot>,
    saved: &SavedBooks<'_>,
    notes: &mut Vec<String>,
) -> Vec<usize> {
    let mut tier_refused: Vec<usize> = Vec::new();
    if snaps.is_empty() {
        return tier_refused;
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
        // The engine's index: 0 the primary, then the extras in order.
        let at = if engine.route_key == *key {
            Some(0)
        } else {
            extras.iter().position(|(_, e)| e.route_key == *key).map(|i| i + 1)
        };
        let Some(at) = at else {
            skipped.push((key, format!("{key}: this core runs no engine with that route key")));
            continue;
        };
        let e = if at == 0 { &mut *engine } else { &mut extras[at - 1].1 };
        if e.venue != snap.venue {
            skipped.push((key, format!("{key}: the journaled engine is venue `{}`", snap.venue)));
            continue;
        }
        // WHOSE book it is. A snapshot that states a tier other than this engine's is not restored
        // (see `refuse_other_tier`); one that states none (written before the field) is restored
        // exactly as it always was, and the next `Snap` states this engine's tier.
        if let Some(snap_tier) = snap.tier
            && snap_tier != e.tier
        {
            refuse_other_tier(e, snap, snap_tier, key, saved, notes);
            tier_refused.push(at);
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
        carry_trading_state(e, snap.trading_state, key, notes);
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
    tier_refused
}

/// **The attribution ledgers of the mounts (`slots`) that trade an engine whose journaled state was
/// refused for its tier are NOT restored** — neither the journal's rows nor the ownership file's.
/// A ledger is the mount's slice of the engine's book (position, realized PnL, fees), and the
/// book it was a slice of was another tier's: restored beside an empty engine it would make the
/// mount hold a position the account does not, and its realized PnL feeds the per-mount budget
/// latch (`watchdog/risk.rs`). The file is told too (a zero `Ledger` record), or its entry, which
/// outranks the journal's, would bring the other tier's numbers back at the next boot.
///
/// Only a ledger that holds something is touched. A ledger parked for a mount that is not mounted yet
/// (`pending_owners`) belongs to no engine this boot knows, so it is left to the runtime mount that
/// takes it over. `log` is `Some` only with `boot_ms` read, the clock rule `assemble_core` keeps.
pub(super) fn forget_refused_mount_ledgers(
    slots: &[usize],
    mount_ids: &[String],
    mount_attr: &mut [MountAttribution],
    mut log: Option<&mut OrderOwnerLog>,
    boot_ms: i64,
    notes: &mut Vec<String>,
) {
    let mut forgotten: Vec<String> = Vec::new();
    for &i in slots {
        if mount_attr[i] == MountAttribution::default() {
            continue;
        }
        mount_attr[i] = MountAttribution::default();
        if let Some(log) = log.as_deref_mut() {
            log.record(OwnerRecord::Ledger {
                mount_id: mount_ids[i].clone(),
                size: 0.0,
                avg_px: 0.0,
                realized_pnl: 0.0,
                fees_paid: 0.0,
                ts: boot_ms,
            });
        }
        forgotten.push(mount_ids[i].clone());
    }
    if !forgotten.is_empty() {
        warn_note(
            notes,
            format!(
                "RESTORE MOUNT LEDGER DROPPED: {} mount(s) trade an engine whose journaled state was \
                 refused for its tier, so their journaled position, PnL and fee ledgers are not \
                 restored either: {}",
                forgotten.len(),
                sample(&forgotten)
            ),
        );
    }
}

impl<C: ExecutionClient> CoreThread<C> {
    /// Give each restored arm its `cond_engine` row back: the engine its journaled ROUTE KEY names,
    /// or — for an arm with none — the engine its owning MOUNT trades (`route_of` over the mount's
    /// route, which defers to the venue for a foreign leg); the mount is
    /// `CoreConfig::conditional_owners`'. An arm with neither keeps no row (the payload route, the
    /// declared residual on `CoreConfig::conditionals`).
    ///
    /// An arm that sits on an engine in `tier_refused` (its snapshot was another tier's book,
    /// [`apply_restored_engines`]) is DISARMED instead, said in one note: it would fire an order
    /// onto an account its journal never armed it on. An arm no mount or route key places is judged
    /// on the engine its payload venue's default account runs, the engine its fire would reach.
    ///
    /// ⚠ **A core-held reduce-only stop (`ArmRoute::reduce_only`) ALWAYS gets its row, or is dropped.**
    /// Its fire is capped to ONE engine's position, so the row is not optional the way a plain arm's is:
    /// a single-account box journals no route key and the arm has no mount, so the lookup above finds
    /// nothing and a plain arm would simply keep the payload route, and the flag with it LOST.
    /// For such an arm the venue's own engine answers instead (`armed_engine`: the sole account, or
    /// engine 0 for a venue this core runs nothing for), and where that is ambiguous (several accounts
    /// of the venue and no route key: a crash-tail arm records none) or the route key names an account
    /// this core does not run, the arm is DISARMED with a note, because a cap judged against another
    /// account's position is the one wrong answer.
    pub(super) fn restore_arm_routes(
        &mut self,
        arms: Vec<ArmRoute>,
        owners: Vec<(String, String)>,
        tier_refused: &[usize],
    ) {
        let owners: std::collections::HashMap<String, String> = owners.into_iter().collect();
        let mut unrouted: Vec<String> = Vec::new();
        let mut other_tier: Vec<String> = Vec::new();
        let mut core_held_unplaced: Vec<String> = Vec::new();
        let mut core_held_restored = 0usize;
        for ArmRoute { arm_id, venue, route_key, reduce_only } in arms {
            let mount =
                owners.get(&arm_id).and_then(|mid| self.mount_ids.iter().position(|id| id == mid));
            let engine = match route_key.as_deref() {
                Some(rk) => self.engine_idx_for_route_key(RouteKey::declared(rk)),
                None => mount.and_then(|slot| self.route_of(EngineRoute::Mount(slot), &venue)),
            };
            let engine = match engine {
                None if reduce_only && route_key.is_none() => self.armed_engine(&arm_id, &venue),
                engine => engine,
            };
            if !tier_refused.is_empty() {
                let judged = match engine {
                    Some(i) => Some(i),
                    None if route_key.is_none() => {
                        self.engine_idx_for_route_key(RouteKey::sole_account_of(&venue))
                    }
                    None => None,
                };
                if judged.is_some_and(|i| tier_refused.contains(&i)) {
                    let book = self.conditional_books.values_mut().find(|b| b.contains(&arm_id));
                    if let Some(book) = book {
                        book.disarm(&arm_id);
                    }
                    other_tier.push(arm_id);
                    continue;
                }
            }
            match engine {
                Some(engine) => {
                    core_held_restored += usize::from(reduce_only);
                    self.cond_engine.insert(arm_id, ArmedBy { engine, mount, reduce_only });
                }
                None if reduce_only => {
                    let book = self.conditional_books.values_mut().find(|b| b.contains(&arm_id));
                    if let Some(book) = book {
                        book.disarm(&arm_id);
                    }
                    core_held_unplaced.push(arm_id);
                }
                None if route_key.is_some() => unrouted.push(arm_id),
                None => {}
            }
        }
        self.refresh_tick_conditionals();
        if core_held_restored > 0 {
            let note = format!(
                "RESTORE: {core_held_restored} core-held reduce-only stop(s) re-armed from the \
                 journal. They protect ONLY while this node runs: a price that crossed one while \
                 the node was down fires on the first tick after it, at whatever the price is."
            );
            tracing::warn!(target: "vike_core::core", "{note}");
            self.note(note);
        }
        if !core_held_unplaced.is_empty() {
            let note = format!(
                "RESTORE ARMS DROPPED: {} core-held reduce-only stop(s) name no account this core can \
                 judge a position against, so they are NOT re-armed (re-arm them): {}",
                core_held_unplaced.len(),
                sample(&core_held_unplaced)
            );
            tracing::warn!(target: "vike_core::core", "{note}");
            self.note(note);
        }
        if !unrouted.is_empty() {
            self.note(format!(
                "RESTORE ARMS UNROUTED: {} restored arm(s) name an account this core runs no \
                 engine for; their fire takes the payload route: {}",
                unrouted.len(),
                sample(&unrouted)
            ));
        }
        if !other_tier.is_empty() {
            let note = format!(
                "RESTORE ARMS DROPPED: {} armed conditional(s) of the journal sit on an engine whose \
                 snapshot was another tier's book, so they are NOT re-armed (a fire would place an \
                 order on an account they were never armed on): {}",
                other_tier.len(),
                sample(&other_tier)
            );
            tracing::warn!(target: "vike_core::core", "{note}");
            self.note(note);
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
