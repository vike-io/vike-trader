//! **The fill-dedup ledger as the journal `Snapshot` carries it: a WINDOW of days, not the whole history.**
//!
//! In the running process the two trade-id sets (the account's money ledger, `Account::seen_fill_ids`, and the
//! engine's FSM set, `seen_fsm_trade_ids`) still keep every id they folded, exactly as before. Each id now also
//! carries the engine DAY it was folded on (the engine clock the journal stamps, `day_of(now_ms)`, so a replay
//! stamps what the live run stamped). A bounded snapshot (`Engine::journal_snapshot`, the one `Snapshot`
//! writer's form) keeps only the ids folded within [`FILL_DEDUP_WINDOW_DAYS`] of the NEWEST id of the same set,
//! and says, when it left any out, the FLOOR: a fill stamped before it cannot be told from a re-delivery of one
//! the window dropped, so a ledger holding the floor refuses it (`Account::apply_fill_on_day`) and the reconcile
//! pass does not read it (`vike_core`'s `reconcile_reports`). The core gives the floor to the running ledger as
//! well each time a `Snapshot` states it (`Account::adopt_fill_floor`), so the process judges a fill as a restart
//! from that `Snapshot` would.
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
//! **Why the window holds.** A venue re-delivers an execution it already sent through four doors: a reconnect's
//! resync, hyperliquid's `userFills` snapshot and the gap sentinel (all floored at the process start upstream,
//! `crates/vike-bridge-core/src/exec_actor.rs`'s `is_pre_spawn`), the gateway's per-account ring (a resume sends
//! only what follows the cursor the `Snapshot` carries, `EventRingCursor::after`), and the reconcile pass's fill read,
//! which reaches back `config.reconcile_lookback_ms`: one hour by default, at most `u32::MAX` ms (49.7 days) by
//! the setting's own type. The window is that longest read-back plus [`FILL_STAMP_SKEW_DAYS`] of stamp skew,
//! rounded up to whole days (the `const` assert below), so every fill a pass may report is either in the window
//! or after the floor, and a re-delivery of a dropped id, stamped as it was the first time, is before it.
//!
//! **Anchored on the newest id, not on a clock**: an account that went quiet keeps the last window of its own
//! activity whatever the date, and a replay keeps exactly what the live run kept (the newest id is never left
//! out, so both see the same anchor). The days ride the `Snapshot` beside the ids, index for index, and are left out
//! of `state_hash`; a `Snapshot` written before them restores every id it lists at its own day.

/// One day of engine clock, ms.
pub const DAY_MS: i64 = 86_400_000;

/// How many days of fold a bounded snapshot keeps in each trade-id set, counted back from the newest id's day
/// (the module doc says why the figure is the longest reconcile read-back plus the skew).
pub const FILL_DEDUP_WINDOW_DAYS: u32 = 51;

/// How far a venue's stamp on a fill may sit from the engine day that folded it, and from the stamp it carries
/// when it is delivered again (a REST row against the stream's frame, a venue clock ahead of the node's).
pub const FILL_STAMP_SKEW_DAYS: u32 = 1;

// The window covers the longest reconcile read-back (`config.reconcile_lookback_ms` is a `u32` of ms) after the
// skew is taken off it: a fill the pass may report is never before the floor.
const _: () = assert!(
    (FILL_DEDUP_WINDOW_DAYS - FILL_STAMP_SKEW_DAYS) as i64 * DAY_MS >= u32::MAX as i64,
    "the fill-dedup window must reach past the longest reconcile read-back"
);

/// The engine day of `now_ms` (0 for a clock before the epoch, the unstamped seed's day).
#[inline]
pub fn day_of(now_ms: i64) -> u32 {
    u32::try_from(now_ms.div_euclid(DAY_MS)).unwrap_or(if now_ms < 0 { 0 } else { u32::MAX })
}

/// The floor a bounded snapshot states when the newest id of its ledger was folded on `newest_day`: the start
/// of the first day the window keeps, plus the skew. Every id it left out was folded before the window, so its
/// re-delivery is stamped before this.
pub fn floor_ms(newest_day: u32) -> i64 {
    i64::from(newest_day.saturating_sub(FILL_DEDUP_WINDOW_DAYS) + FILL_STAMP_SKEW_DAYS) * DAY_MS
}

/// One trade-id set as a snapshot writes it: the ids SORTED (the canonical form `state_hash` reads), their fold
/// days beside them, and whether the window left an id out.
pub(crate) struct FillWindowIds {
    pub(crate) ids: Vec<String>,
    pub(crate) days: Vec<u32>,
    pub(crate) left_out: bool,
    pub(crate) newest: Option<u32>,
}

/// `entries` as a snapshot writes them: every one, or (`bounded`) the ones folded within the window of the
/// newest. Cold: once per snapshot, never on the fold.
pub(crate) fn windowed<'a>(
    entries: impl Iterator<Item = (&'a str, u32)>,
    bounded: bool,
) -> FillWindowIds {
    let mut all: Vec<(&str, u32)> = entries.collect();
    let newest = all.iter().map(|&(_, d)| d).max();
    let len = all.len();
    if bounded && let Some(n) = newest {
        let first = n.saturating_sub(FILL_DEDUP_WINDOW_DAYS);
        all.retain(|&(_, d)| d >= first);
    }
    let left_out = all.len() < len;
    all.sort_unstable_by(|a, b| a.0.cmp(b.0));
    let days = all.iter().map(|&(_, d)| d).collect();
    let ids = all.into_iter().map(|(id, _)| id.to_string()).collect();
    FillWindowIds { ids, days, left_out, newest }
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
