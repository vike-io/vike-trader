//! **The fill-dedup ledger as the journal `Snapshot` carries it: a WINDOW of days, not the whole history.**
//!
//! In the running process the two trade-id sets (the account's money ledger, `Account::seen_fill_ids`, and the
//! engine's FSM set, `seen_fsm_trade_ids`) still keep every id they folded, exactly as before. Each id now also
//! carries the engine DAY it was folded on (the engine clock the journal stamps, `day_of(now_ms)`, so a replay
//! stamps what the live run stamped). A bounded snapshot (`Engine::journal_snapshot`, the one `Snapshot`
//! writer's form) keeps only the ids folded within the engine's window ([`window_days`]) of the NEWEST id of the
//! same set, and says, when it left any out, the FLOOR: a fill stamped before it cannot be told from a re-delivery
//! of one the window dropped, so a ledger holding the floor refuses it (`Account::apply_fill_on_day`), the order
//! FSM refuses its wrap the same way (`Engine::on_event`'s lifecycle arm: an order still resting can re-list a fill
//! whose id the FSM set no longer holds, and `accumulate_fill` would count it twice), and the reconcile pass does
//! not read it (`vike_core`'s `reconcile_reports`). The core gives the floor to the running
//! ledger as well each time a `Snapshot` states it (`Account::adopt_fill_floor`), so the process judges a fill as a
//! restart from that `Snapshot` would.
//!
//! **The FSM set rides as its DIFFERENCE from the account's** ([`difference`]): in steady state every fill comes as
//! a bare fill and its wrap, so the two sets hold the same ids, and a second full copy doubled the record. The
//! `Snapshot` writes the account's ids, the FSM's entries the account does not hold at the same day
//! (`EngineSnapshot::fsm_only_trade_ids`: a wrap whose bare fill was refused, a reconcile-adopted fill) and the
//! account's ids the FSM does not hold (`EngineSnapshot::account_only_trade_ids`: a bare fill whose wrap has not
//! come, a bare fill for an order the engine does not hold). A restore rebuilds both sets exactly, each id at its
//! day.
//!
//! **A fill with NO timestamp is never booked under a floor.** A venue frame that omits the time field parses
//! as `ts` 0 (`vike_bridge_core::json`'s `get_i64`). No bridge builds a bare fill with a literal 0, and every
//! venue's own fill stream and history carry a stamp; a 0 is a malformed frame. Nothing then proves such a fill
//! new, so once the ledger holds a floor a no-stamp fill whose id it does not hold is refused
//! (`FillFold::Unproven`) and parked for the operator, who sees it as a held `MissingFill` row (`vike_core`'s
//! `hold_unproven_fills`); a confirm of the row books nothing. Before any floor (every ledger younger than the
//! window) such a fill books as it always did. The cost, stated: a reconcile-synthesized leg that carries the
//! 0 of its venue row (alpaca's position rows state no time) is held the same way once a floor stands.
//!
//! **Why any window is safe, and what its size buys.** A re-delivered execution carries the stamp it had the first
//! time, so a re-delivery of an id the window dropped is stamped before the floor and refused, whatever the
//! window: the floor, not the window's size, is what keeps a fill from moving money twice. The size decides REACH:
//! a genuinely missed fill stamped before the floor is not booked either. A venue re-delivers an execution through
//! four doors: a reconnect's resync, hyperliquid's `userFills` snapshot and the gap sentinel (all floored at the
//! process start upstream, `crates/vike-bridge-core/src/exec_actor.rs`'s `is_pre_spawn`), the gateway's per-account
//! ring (a resume sends only what follows the cursor the `Snapshot` carries, `EventRingCursor::after`), and the
//! reconcile pass's fill read, which reaches back the node's `config.reconcile_lookback_ms` (one hour by default).
//! So the window is that read-back, rounded up to whole days, plus [`FILL_STAMP_SKEW_DAYS`] of stamp skew and
//! [`FILL_WINDOW_MARGIN_DAYS`] ([`window_days`]): every fill a pass may report is either in the window or after the
//! floor. The core sizes each engine's window from the row (`vike_core::CoreConfig::reconcile_lookback_ms`,
//! `Engine::size_fill_window`); an engine nobody sized keeps [`WIDEST_WINDOW_DAYS`], the longest read-back the
//! setting's `u32` type allows.
//!
//! **A read-back RAISED past the window a restored `Snapshot` was cut to still books nothing twice.** The restored
//! ledger keeps that `Snapshot`'s floor and a floor only moves forward (`Account::seed_fill_ledger`,
//! `Account::fill_ledger`), so a pass reaching further back drops every row stamped before it, a re-delivery and a
//! genuinely missed fill alike; the window then grows back to its new size as new fills fold.
//!
//! **Anchored on the newest id, not on a clock**: an account that went quiet keeps the last window of its own
//! activity whatever the date, and a replay keeps exactly what the live run kept (the newest id is never left
//! out, so both see the same anchor, and the window rides the `Snapshot` as `EngineSnapshot::fill_window_days`). The
//! days ride the `Snapshot` beside the ids, index for index, and are left out of `state_hash`; a `Snapshot` written
//! before them restores every id it lists at its own day.

use std::cmp::Ordering;

use vike_model::{MS_PER_DAY, MS_PER_DAY_U64};

/// How far a venue's stamp on a fill may sit from the engine day that folded it, and from the stamp it carries
/// when it is delivered again (a REST row against the stream's frame, a venue clock ahead of the node's).
pub const FILL_STAMP_SKEW_DAYS: u32 = 1;

// Two days of headroom past the read-back, for the reads that take no start time (bybit's `/v5/execution/list` on a
// gap-sentinel resync lists up to 7 days, row-count-bounded history endpoints): a genuinely missed fill one of them
// returns from just past the read-back is judged by its id instead of refused by the floor.
/// The reach a window keeps beyond the reconcile read-back and the skew ([`window_days`]).
pub const FILL_WINDOW_MARGIN_DAYS: u32 = 2;

/// How many days of fold a bounded snapshot keeps in each trade-id set, counted back from the newest id's day,
/// on a node whose reconcile pass reads `lookback_ms` back: the read-back in whole days, the skew and the margin
/// (the module doc says why).
pub const fn window_days(lookback_ms: i64) -> u32 {
    // `config.reconcile_lookback_ms` is a `u32` of ms: never more than 50 days.
    let lookback = if lookback_ms <= 0 {
        0
    } else if lookback_ms > u32::MAX as i64 {
        u32::MAX as u64
    } else {
        lookback_ms as u64
    };
    lookback.div_ceil(MS_PER_DAY_U64) as u32 + FILL_STAMP_SKEW_DAYS + FILL_WINDOW_MARGIN_DAYS
}

/// The window of an engine nobody sized (`Engine::size_fill_window`): the longest read-back the setting's type
/// allows, so no pass reaches past it.
pub const WIDEST_WINDOW_DAYS: u32 = window_days(u32::MAX as i64);

/// The engine day of `now_ms` (0 for a clock before the epoch, the unstamped seed's day).
#[inline]
pub fn day_of(now_ms: i64) -> u32 {
    u32::try_from(now_ms.div_euclid(MS_PER_DAY)).unwrap_or(if now_ms < 0 { 0 } else { u32::MAX })
}

/// The floor a bounded snapshot of `window` days states when the newest id of its ledger was folded on
/// `newest_day`: the start of the first day the window keeps, plus the skew. Every id it left out was folded
/// before the window, so its re-delivery is stamped before this.
pub fn floor_ms(newest_day: u32, window: u32) -> i64 {
    i64::from(newest_day.saturating_sub(window) + FILL_STAMP_SKEW_DAYS) * MS_PER_DAY
}

/// One trade-id set as a snapshot writes it: its entries SORTED by id (the canonical form `state_hash` reads),
/// each with its fold day, and whether the window left an id out.
pub(crate) struct FillWindowIds<'a> {
    pub(crate) entries: Vec<(&'a str, u32)>,
    pub(crate) left_out: bool,
    pub(crate) newest: Option<u32>,
}

/// `entries` as a snapshot writes them: every one (`window` `None`), or the ones folded within `window` days of
/// the newest. Cold: once per snapshot, never on the fold.
pub(crate) fn windowed<'a>(
    entries: impl Iterator<Item = (&'a str, u32)>,
    window: Option<u32>,
) -> FillWindowIds<'a> {
    let mut all: Vec<(&str, u32)> = entries.collect();
    let newest = all.iter().map(|&(_, d)| d).max();
    let len = all.len();
    if let (Some(days), Some(n)) = (window, newest) {
        let first = n.saturating_sub(days);
        all.retain(|&(_, d)| d >= first);
    }
    let left_out = all.len() < len;
    all.sort_unstable_by(|a, b| a.0.cmp(b.0));
    FillWindowIds { entries: all, left_out, newest }
}

/// The FSM set as a snapshot writes it beside the account's (both SORTED by id, as [`windowed`] leaves them): the
/// FSM entries the account does not hold, or holds at another day (`fsm_only`), and the account's ids the FSM does
/// not hold (`account_only`), both sorted. `by_day` false compares ids alone: a form that writes no days restores
/// every id at one day, so a day apart could not come back.
pub(crate) fn difference<'a>(
    account: &[(&'a str, u32)],
    fsm: &[(&'a str, u32)],
    by_day: bool,
) -> (Vec<(&'a str, u32)>, Vec<&'a str>) {
    let (mut fsm_only, mut account_only) = (Vec::new(), Vec::new());
    let (mut i, mut j) = (0, 0);
    loop {
        let order = match (account.get(i), fsm.get(j)) {
            (None, None) => break,
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (Some(a), Some(f)) => a.0.cmp(f.0),
        };
        match order {
            Ordering::Less => {
                account_only.push(account[i].0);
                i += 1;
            }
            Ordering::Greater => {
                fsm_only.push(fsm[j]);
                j += 1;
            }
            Ordering::Equal => {
                if by_day && account[i].1 != fsm[j].1 {
                    fsm_only.push(fsm[j]);
                }
                i += 1;
                j += 1;
            }
        }
    }
    (fsm_only, account_only)
}

/// `entries` as the `Snapshot`'s two lists: the ids, and their days index for index.
pub(crate) fn written(entries: &[(&str, u32)]) -> (Vec<String>, Vec<u32>) {
    entries.iter().map(|&(id, day)| (id.to_string(), day)).unzip()
}

/// The `(id, day)` pairs a snapshot's `ids` and `days` restore: index for index, and every id at `fallback_day`
/// when the days are absent or do not line up (a `Snapshot` written before them).
pub(crate) fn restored<'a>(
    ids: &'a [String],
    days: &'a [u32],
    fallback_day: u32,
) -> impl Iterator<Item = (String, u32)> + 'a {
    let lined_up = days.len() == ids.len();
    ids.iter()
        .enumerate()
        .map(move |(i, id)| (id.clone(), if lined_up { days[i] } else { fallback_day }))
}
