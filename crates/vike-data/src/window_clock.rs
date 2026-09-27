//! [`WindowClock`] — WHICH CLOCK a Polymarket backtest window is scoped on, and the counter that
//! measures the resulting disagreement from inside a single run.
//!
//! It lives in its own module rather than beside the selector (`crate::backtest_store`)
//! because the selector is gated behind the `vike-archive` feature (it names
//! `crate::archive_store`), and the divergence this module measures is a property of the RECORDER's
//! tape rather than of any one store — so the ONE authority for it stays reachable without the
//! feature. This module depends on nothing but `crate::TsRange`.
//!
//! ⚠ **There is no `local_ts`-scoped store left in this tree, and that is a 2026-09-20 change.**
//! The reason this module was UNGATED used to be that `backtest_bridge`'s `ClickHousePolyHistStore`
//! — the one store that scoped its window on `local_ts` — compiled unconditionally while the
//! selector did not. That store is DELETED (data is fetched by API or from the venue directly,
//! never by reaching ClickHouse); `crates/vike-data/src/backtest_store.rs`'s module doc
//! carries what went with it. [`WindowClock::LocalTs`] therefore names no store now — it names the
//! recorder's OTHER stamp, which is still on every row both surviving stores read, and which
//! [`other_clock_excludes`] is still the honesty counter for.

use crate::TsRange;

/// WHICH CLOCK a store's `TsRange` is scoped on — the one authority for the divergence between the
/// recorder's two stamps, and the reason a run recorded against the retired `local_ts`-scoped
/// ClickHouse store is not comparable, over "the same window", with one produced today.
///
/// The recorder stamps every row twice. `ts` is the venue's own "this state is AS OF" number;
/// `local_ts` is "we READ this frame off the socket AT". `crates/bridges/polymarket/src/
/// market_feed.rs`'s `handle_update` is the writer of both: `ts` is the frame's own stamp and falls
/// back to the local clock when the frame carries none (`ts.unwrap_or_else(now_ms)`), so `ts` is
/// never 0 and never a third "unset" state — the hazard
/// `crates/vike-data/src/store_kind.rs`'s `quote` note carries for the hist store's own `local_ts`
/// column ("0 in any part sealed before the column existed") does NOT exist on this tape, and was
/// checked rather than assumed (0 of 521,257,024 rows below).
///
/// **MEASURED 2026-09-20 against the deployed recorder tape, one UTC day (2026-09-18,
/// 521,257,024 `polymarket.book_events` rows), lag = `local_ts - ts`:**
///
/// | `event_type` | rows | p50 | p99 | max |
/// |---|---|---|---|---|
/// | `price_change` | 513,551,290 | 18 ms | 956 ms | 8,969 ms |
/// | `book` (the snapshot anchors) | 3,939,961 | 27 ms | 362,586 ms | 151,028,189 ms = 41.9 h |
/// | `status` | 2,761,964 | 0 ms | 0 ms | 0 ms |
/// | `trade` | 1,003,809 | 24 ms | 1,557 ms | 7,993 ms |
///
/// Lag was never negative (min 0 over the day), so `local_ts >= ts` held everywhere measured.
///
/// ⚠ **The divergence is NOT "the feed latency", and a brief that says so is wrong.** 98.5% of the
/// tape is `price_change`, where it really is tens of milliseconds. It is the `book` rows — the
/// full-ladder SNAPSHOT anchors a replay seeds from — that diverge by hours, because a freshly
/// subscribed token's first snapshot carries the market's LAST venue update as its `ts` while
/// arriving now.
///
/// **What that costs at the window size these bins actually run.**
/// `crates/vike-poly-research/src/main.rs`'s `window_for` is a FIVE-minute (15 for the
/// `-15m` family) slice — one rolling market's whole life. Bucketing that day's `book` rows into
/// 5-minute cells per token under each clock: 1,081,821 `(token, cell)` pairs hold an anchor under
/// at least one clock, 928,520 under both, **132,694 (12.3%) under `local_ts` ONLY**, and 20,607
/// (1.9%) under `ts` only. So for roughly one market-window in eight the `ts`-scoped stores see no
/// snapshot anchor where the `local_ts`-scoped one does — and
/// `crates/vike-data/src/archive_store.rs`'s `scan_quotes` derived-L1 fold is ANCHOR-GATED, so
/// such a window yields no quotes at all. That is the failure whose measured cost is already
/// written at that verb (-2.74% reported against -27.64% true).
///
/// At a UTC-DAY window the same divergence is 4,521 rows out of 521M (0.00087%): 2,025 rows in the
/// `local_ts` window whose `ts` falls before it, 2,496 rows arriving the next day whose `ts` falls
/// inside it. A day-window comparison therefore says almost nothing about a five-minute one.
///
/// **Why they could not simply be made to agree** — kept because it is the argument that made this
/// a NAMED difference rather than a fix, and because the tape it describes is the one the surviving
/// stores read. The recorder's deployed table is `PARTITION BY toYYYYMMDD(local_ts_dt)` /
/// `ORDER BY (token_id, local_ts, seq)` — read off its own `SHOW CREATE TABLE`. A `ts`-only
/// predicate prunes no partition and skips no granule on that axis, so an exact `ts` window over
/// one token read that token's whole history instead of one day; and no bound relates the two
/// columns (the 41.9 h above is a measurement, not a ceiling), so no cushion widening a `local_ts`
/// predicate could be PROVEN not to drop rows. Both available "fixes" therefore either regressed
/// the export or silently lost data — and silently losing data is exactly what this crate has
/// already paid for once.
///
/// **The same question on the `l1_quotes` table. MEASURED on the latency box 2026-09-20**
/// (`polymarket.l1_quotes`, the UTC day 2026-09-18, 70,269,310 rows in the `local_ts` window):
/// 1,702 carry a `ts` outside the day and 1,799 rows arriving after it closed carry a `ts` inside
/// it — 3,501 rows of symmetric difference, 0.0050% of the day. `countIf(ts = 0)` is 0 here too.
/// Lag over that day: min 12 ms, p50 18 ms, p99 6,116 ms, max 151,028,189 ms — the same 41.95 h
/// maximum, which is what a derived L1 row inherits from the snapshot that produced it. The
/// deployed shape is `PARTITION BY toYYYYMMDD(local_ts_dt)` / `ORDER BY (token_id, local_ts)` — no
/// `seq`, unlike `book_events`.
///
/// ⚠ **Two of the four sites this doc compared are GONE, and the numbers are kept anyway.** A
/// ClickHouse READ path (`backtest_bridge`'s `quotes_query`, the only `local_ts`-scoped one) and a
/// ClickHouse INGEST export (`clickhouse_poly`'s `l1_quotes_query`, `ts`-scoped and pinned there by
/// a different property again — it projected its window column as the store row's ONLY stamp, and
/// committed per `(kind, token, day)`) were deleted on 2026-09-20. The measurements survive because
/// they are facts about the recorder's TAPE, which both surviving stores still read through the
/// archive; what no longer exists is a second in-tree store to disagree with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowClock {
    /// The venue's own stamp. `crate::archive_store::ArchiveParquetHistStore` and
    /// `crate::DataFusionHist` — i.e. every store a Polymarket backtest can be pointed at
    /// today, which is why `crate::backtest_store` no longer has a clock CHOICE to make.
    Ts,
    /// The recorder's socket-read stamp. ⚠ NO STORE IN THIS TREE SCOPES ON IT any more: the one
    /// that did (`backtest_bridge`'s `ClickHousePolyHistStore`) was deleted on 2026-09-20. The
    /// variant stays because the COLUMN does — it is on every row the archive serves, it is what
    /// [`other_clock_excludes`] measures a returned row against, and it is what makes a number
    /// recorded before that date incomparable with one recorded after.
    LocalTs,
}

impl WindowClock {
    /// The column name, as the store's own filter spells it.
    pub fn column(self) -> &'static str {
        match self {
            WindowClock::Ts => "ts",
            WindowClock::LocalTs => "local_ts",
        }
    }

    /// The one-phrase gloss an operator-facing line uses.
    pub fn gloss(self) -> &'static str {
        match self {
            WindowClock::Ts => "the venue's own as-of stamp",
            WindowClock::LocalTs => "when the recorder read the frame off the socket",
        }
    }

    /// The OTHER clock — the one the sibling stores scope on, and therefore the one
    /// [`other_clock_excludes`] measures a returned row against.
    pub fn other(self) -> Self {
        match self {
            WindowClock::Ts => WindowClock::LocalTs,
            WindowClock::LocalTs => WindowClock::Ts,
        }
    }
}

/// How many of `rows` — each `(ts, local_ts)` — the OTHER clock would have excluded from `range`.
/// Pure; the honesty counter behind
/// `crates/vike-data/src/archive_store.rs`'s `scan_book_updates` per-scan warning.
///
/// A non-zero answer is the proof that THIS window is one of the ones the two CLOCKS disagree
/// about, available from inside a single run. It can only ever UNDER-report: a store cannot count
/// the rows its own filter never fetched, so it sees one direction of the symmetric difference.
/// Both directions are measured on [`WindowClock`]; this counts the one a live scan can see.
///
/// ⚠ It took a `clock` parameter because two stores called it with opposite values. Only the
/// `ts`-scoped side is left (the `local_ts`-scoped store was deleted 2026-09-20), so today every
/// caller passes [`WindowClock::Ts`]. The parameter stays — collapsing it would bake one clock into
/// the counter, and the tests below are what keep both directions proven.
pub fn other_clock_excludes(
    clock: WindowClock,
    rows: impl IntoIterator<Item = (i64, i64)>,
    range: TsRange,
) -> usize {
    let start = range.start.unwrap_or(i64::MIN);
    let end = range.end.unwrap_or(i64::MAX);
    rows.into_iter()
        .filter(|&(ts, local_ts)| {
            let other = match clock {
                WindowClock::Ts => local_ts,
                WindowClock::LocalTs => ts,
            };
            other < start || other > end
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two clocks are each other's `other`, and each spells the column the recorder's tape
    /// actually names — the strings the archive store's warning and the run banner are built from.
    #[test]
    fn each_clock_names_its_column_and_its_opposite() {
        assert_eq!(WindowClock::Ts.column(), "ts");
        assert_eq!(WindowClock::LocalTs.column(), "local_ts");
        assert_eq!(WindowClock::Ts.other(), WindowClock::LocalTs);
        assert_eq!(WindowClock::LocalTs.other(), WindowClock::Ts);
        assert_ne!(WindowClock::Ts.gloss(), WindowClock::LocalTs.gloss());
    }

    /// `other_clock_excludes` counts exactly the rows the OTHER clock would have dropped, on either
    /// clock, and is blind to rows the scan never fetched.
    ///
    /// ⚠ Both directions are asserted although only the `Ts` one has a caller today. That is the
    /// point: the `local_ts` direction is what a number recorded before the ClickHouse store was
    /// deleted was produced under, and a counter that could no longer compute it would make the two
    /// runs silently look comparable.
    #[test]
    fn other_clock_excludes_counts_the_visible_half_of_the_difference() {
        // (ts, local_ts): in-window on ts, but arrival 41.9 h later — the real `book`-row shape.
        let rows = [(150i64, 150_907_000i64), (150, 160), (100, 100), (200, 250)];
        // A `ts`-scoped store returns all four; a `local_ts`-scoped window would have dropped
        // the stale-anchor row and the one arriving after the window closes.
        assert_eq!(other_clock_excludes(WindowClock::Ts, rows, TsRange::of(100, 200)), 2);
        // A `local_ts`-scoped window would instead be flagging rows whose `ts` fell outside it —
        // none of these do.
        assert_eq!(other_clock_excludes(WindowClock::LocalTs, rows, TsRange::of(100, 200)), 0);
        // An unbounded range can exclude nothing on either clock.
        assert_eq!(other_clock_excludes(WindowClock::Ts, rows, TsRange::all()), 0);
        assert_eq!(other_clock_excludes(WindowClock::LocalTs, rows, TsRange::all()), 0);
    }
}
