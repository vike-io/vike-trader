//! The gap-heal PLANNER: gap set (+ the freshness lookback) → the concrete backfill jobs one pass
//! over one symbol would dispatch. Pure — it never touches the store, the clock or the network, so
//! "what would the supervisor do about these holes" is fully unit-testable.
//!
//! The gap set itself comes from `vike_data::DataFusionHist::series_gaps` (manifest-derived missing
//! UTC days as inclusive `[start_ms, end_ms]` epoch-ms ranges, ascending, no Parquet scan). This
//! module only decides what to DO with it; [`super::run`] is what calls `series_gaps` and then
//! dispatches what is planned here through the registry.
//!
//! ORDER IS LOAD-BEARING: the freshness job comes FIRST, gap-heal after. An always-on collector's
//! primary duty is that "now" is present; healing 2019 must never delay today's bars.
//!
//! THREE INVARIANTS THIS MODULE EXISTS TO HOLD (each of them a bug the naive shape has):
//!
//! 1. **The freshness window is WATERMARK-ANCHORED, not a raw rolling `[now - lookback, now]`.** The
//!    store's idempotency is BATCH-COMMIT-KEY dedup — `{venue}:{symbol}:{interval}:{start}-{end}`,
//!    NEVER per-row value dedup — so a window whose BOTH ends move with the clock mints a brand-new
//!    key every pass and re-appends the same bars forever (a 24h lookback on a 5-minute cadence =
//!    ~288x row amplification). [`fresh_window`] therefore starts at `max(now - lookback,
//!    last_ts + step)` where `last_ts` is the series' stored watermark
//!    (`DataFusionHist::series_coverage().last_ts`): a healthy series only ever fetches the sliver
//!    since its last stored bar, and the `lookback` is just the OUTER bound for a cold or
//!    long-stalled series. When the watermark is already at/after "now" there is nothing to fetch
//!    and NO freshness job is planned at all (`None`).
//! 2. **Heal work ROTATES.** A gap the venue genuinely has no data for can never be closed: the
//!    fetch returns 0 rows and `commit_rows` early-returns BEFORE recording the commit key, so the
//!    manifest never gains those days and `series_gaps` re-reports the identical hole forever. Always
//!    taking the oldest `max_heal_jobs` ranges would then re-attempt those same N holes every pass
//!    and never reach hole N+1. [`plan_heal_jobs`] takes a rotating `cursor` instead, so successive
//!    passes walk the whole gap set round-robin.
//! 3. **`max_heal_jobs` bounds WORK, not range count.** `vike_data::find_gaps` collapses consecutive
//!    missing days into ONE range, so a 300-day contiguous hole is a single range spanning ~432k 1m
//!    bars (~432 paged REST calls) — one "job" that blocks the pass, and with it `stop()`, for
//!    minutes. Every range is therefore split into [`HEAL_CHUNK_MS`] sub-windows BEFORE the
//!    `max_heal_jobs` cap applies.

use super::config::SourceConfig;

/// Longest span a single heal job may cover: 7 days. A gap range wider than this is split into
/// consecutive sub-windows so `max_heal_jobs` is a genuine per-pass work bound (invariant 3 above) —
/// otherwise one contiguous multi-year hole would be one unbounded job and `stop()` would block for
/// its whole duration. Chosen to stay well inside every wired collector's paging comfort while still
/// making progress on a big hole in few passes.
pub const HEAL_CHUNK_MS: i64 = 7 * 86_400_000;

/// Why a job was planned — the status surface counts `Heal` jobs as the heal queue depth, and the
/// distinction makes the planner's ordering assertable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobReason {
    /// The watermark-anchored freshness window (see [`fresh_window`]). This is the lane whose
    /// failures drive the source's backoff — a failing HEAL job must never slow "now" down.
    Fresh,
    /// One manifest-detected hole in the series' recorded span.
    Heal,
}

/// One concrete backfill dispatch: what [`super::registry::BackfillFn`] will be called with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealJob {
    pub symbol: String,
    pub interval: String,
    /// Inclusive window start (epoch-ms) — same convention as `series_gaps`/`TsRange`.
    pub start_ms: i64,
    /// Inclusive window end (epoch-ms).
    pub end_ms: i64,
    pub reason: JobReason,
}

/// The freshness window ending at `now_ms`, anchored on the series' stored watermark:
/// `[max(now - lookback, last_ts + step), now]`.
///
/// `last_ts_ms` is `DataFusionHist::series_coverage().last_ts` for this series (0 for a series with
/// no parts yet, and 0 is also the right fallback when the coverage read fails — the window then
/// degrades to the plain `lookback` bound). `step_ms` is the bar interval in ms, so the window starts
/// at the FIRST not-yet-stored bar rather than re-covering the stored one.
///
/// Returns `None` when the anchor has already passed `now_ms` (nothing new can exist yet) — the
/// caller plans no freshness job at all. This is what keeps the commit key from moving on a series
/// that is already current: the key's `start` only advances when real bars land.
///
/// A negative `lookback_ms` is floored at 0 (config validation already rejects it; this is the
/// defensive twin), `step_ms` is floored at 1ms (an unparseable interval must still not re-cover the
/// stored watermark bar), and every arithmetic op saturates.
pub fn fresh_window(
    now_ms: i64,
    lookback_ms: i64,
    last_ts_ms: i64,
    step_ms: i64,
) -> Option<(i64, i64)> {
    let bound = now_ms.saturating_sub(lookback_ms.max(0));
    let start = bound.max(last_ts_ms.saturating_add(step_ms.max(1)));
    (start <= now_ms).then_some((start, now_ms))
}

/// Split one inclusive `[start, end]` range into consecutive `<= HEAL_CHUNK_MS` sub-windows,
/// appending them to `out`. A degenerate `start > end` range (which a corrupted manifest could
/// produce) contributes nothing.
fn chunk_range(start_ms: i64, end_ms: i64, out: &mut Vec<(i64, i64)>) {
    if start_ms > end_ms {
        return;
    }
    let mut s = start_ms;
    loop {
        let e = s.saturating_add(HEAL_CHUNK_MS - 1).min(end_ms);
        out.push((s, e));
        if e >= end_ms {
            return;
        }
        s = e.saturating_add(1);
    }
}

/// Every gap range split into [`HEAL_CHUNK_MS`] sub-windows, ascending, degenerate ranges dropped.
/// This — not the raw range list — is what `max_heal_jobs` is counted against.
pub fn chunked_gaps(gaps: &[(i64, i64)]) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    for &(start_ms, end_ms) in gaps {
        chunk_range(start_ms, end_ms, &mut out);
    }
    out
}

/// Map a series' gap ranges to at most `max_jobs` heal jobs, starting at `cursor` and wrapping —
/// the rotation that makes a permanently-empty hole cost one slot per pass instead of blocking
/// every later hole forever (invariant 2 in the module doc).
///
/// `cursor` is a free-running per-source counter; it is reduced modulo the CHUNK count here rather
/// than by the caller, because two symbols of the same source have different chunk counts and must
/// each wrap on their own. `max_jobs == 0` or an empty gap set plans nothing, and no chunk is ever
/// planned twice in one pass (the take is clamped to the chunk count).
pub fn plan_heal_jobs(
    symbol: &str,
    interval: &str,
    gaps: &[(i64, i64)],
    max_jobs: usize,
    cursor: usize,
) -> Vec<HealJob> {
    let chunks = chunked_gaps(gaps);
    if chunks.is_empty() || max_jobs == 0 {
        return Vec::new();
    }
    let skip = cursor % chunks.len();
    let take = max_jobs.min(chunks.len());
    chunks
        .iter()
        .cycle()
        .skip(skip)
        .take(take)
        .map(|&(start_ms, end_ms)| HealJob {
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            start_ms,
            end_ms,
            reason: JobReason::Heal,
        })
        .collect()
}

/// The whole per-symbol plan for one pass: the freshness window first (when there is anything new to
/// fetch at all), then (when `src.heal`) the bounded, rotating gap-heal jobs.
///
/// `gaps` is what `series_gaps` returned for this symbol's series — pass an empty slice when healing
/// is off or the lookup failed. `last_ts_ms` is the series watermark ([`fresh_window`]); pass 0 when
/// unknown. `heal_cursor` is the source's rotating heal cursor ([`plan_heal_jobs`]).
pub fn plan_pass(
    src: &SourceConfig,
    symbol: &str,
    gaps: &[(i64, i64)],
    now_ms: i64,
    last_ts_ms: i64,
    heal_cursor: usize,
) -> Vec<HealJob> {
    // Validation guarantees the interval parses; `unwrap_or(1)` is the defensive twin for a
    // hand-built config that skipped it — 1ms still keeps the window off the stored watermark bar.
    let step_ms = vike_model::time::interval_ms(&src.interval).unwrap_or(1);
    let mut jobs = Vec::new();
    if let Some((start_ms, end_ms)) = fresh_window(now_ms, src.lookback_ms, last_ts_ms, step_ms) {
        jobs.push(HealJob {
            symbol: symbol.to_string(),
            interval: src.interval.clone(),
            start_ms,
            end_ms,
            reason: JobReason::Fresh,
        });
    }
    if src.heal {
        jobs.extend(plan_heal_jobs(
            symbol,
            src.interval.as_str(),
            gaps,
            src.max_heal_jobs,
            heal_cursor,
        ));
    }
    jobs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::config::parse_supervisor_config;

    /// A one-source config built through the REAL parser, so the planner tests exercise the same
    /// defaults an operator's file would get.
    fn source(extra: &str) -> SourceConfig {
        let text = format!(
            r#"
[[source]]
name = "t"
collector = "binance_klines"
symbols = ["BTCUSDT"]
interval = "1m"
{extra}
"#
        );
        let mut cfg = parse_supervisor_config(&text).expect("test config parses");
        cfg.sources.remove(0)
    }

    /// A realistic epoch-ms "now" — the planner's watermark anchor is an absolute stamp, so toy
    /// values like `10_000` would sit before the very first bar step.
    const NOW: i64 = 1_700_000_000_000;
    const STEP_1M: i64 = 60_000;

    #[test]
    fn fresh_window_spans_the_lookback_when_the_series_is_cold() {
        // last_ts = 0 (no parts yet) → the lookback bound is what binds.
        assert_eq!(fresh_window(10_000, 3_000, 0, 1), Some((7_000, 10_000)));
        assert_eq!(fresh_window(10_000, 0, 0, 1), Some((10_000, 10_000)));
        assert_eq!(
            fresh_window(10_000, -5, 0, 1),
            Some((10_000, 10_000)),
            "negative lookback floored at 0"
        );
        assert_eq!(
            fresh_window(NOW, 86_400_000, 0, STEP_1M),
            Some((NOW - 86_400_000, NOW)),
            "a cold series fetches the whole lookback"
        );
    }

    #[test]
    fn fresh_window_anchors_on_the_stored_watermark_when_that_is_newer() {
        // Two stored minutes back → only those two minutes are refetched, NOT the whole 24h.
        assert_eq!(
            fresh_window(NOW, 86_400_000, NOW - 120_000, STEP_1M),
            Some((NOW - 60_000, NOW)),
            "start = last_ts + one bar step"
        );
    }

    #[test]
    fn a_series_already_current_plans_no_window_at_all() {
        // THE ANTI-DUPLICATE PIN. The store dedups by BATCH COMMIT KEY
        // (`{venue}:{symbol}:{interval}:{start}-{end}`), never by row value, so a window whose start
        // moves with the clock would mint a new key — and re-append the same bars — every pass. An
        // anchored start that has already passed "now" means there is nothing to fetch: no job, no
        // key, no rows.
        assert_eq!(fresh_window(NOW, 86_400_000, NOW, STEP_1M), None);
        assert_eq!(fresh_window(NOW, 86_400_000, NOW - 1, STEP_1M), None);
        assert_eq!(fresh_window(NOW, 86_400_000, i64::MAX, STEP_1M), None, "saturating anchor");
        assert_eq!(fresh_window(i64::MIN, 1_000, i64::MIN, 1), None, "saturating bound");
    }

    #[test]
    fn the_freshness_window_never_recovers_an_already_stored_bar() {
        let lookback = 86_400_000;
        // Pass 1, cold series: the full lookback bound.
        let (s1, e1) = fresh_window(NOW, lookback, 0, STEP_1M).expect("cold series has work");
        assert_eq!((s1, e1), (NOW - lookback, NOW));
        // That pass stored bars through `NOW - STEP_1M` (the last CLOSED one). Pass 2, 5 minutes
        // later, starts exactly at the first bar that pass could not have stored.
        let (s2, e2) =
            fresh_window(NOW + 300_000, lookback, NOW - STEP_1M, STEP_1M).expect("new bars exist");
        assert_eq!((s2, e2), (NOW, NOW + 300_000));
        assert!(s2 > NOW - STEP_1M, "strictly past the stored watermark");
        assert!(s2 > e1 - lookback, "the start moved forward, so the commit key covers new ground");
    }

    #[test]
    fn no_gaps_plans_only_the_freshness_job() {
        let src = source("lookback_ms = 3600000");
        let jobs = plan_pass(&src, "BTCUSDT", &[], NOW, 0, 0);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].reason, JobReason::Fresh);
        assert_eq!(jobs[0].symbol, "BTCUSDT");
        assert_eq!(jobs[0].interval, "1m");
        assert_eq!(jobs[0].start_ms, NOW - 3_600_000);
        assert_eq!(jobs[0].end_ms, NOW);
    }

    #[test]
    fn a_current_series_plans_heal_work_only() {
        // Freshness has nothing to do, but the historical holes still get their bounded budget —
        // dropping the fresh job must not drop the pass.
        let src = source("lookback_ms = 3600000");
        assert!(plan_pass(&src, "BTCUSDT", &[], NOW, NOW, 0).is_empty());
        let jobs = plan_pass(&src, "BTCUSDT", &[(1, 2)], NOW, NOW, 0);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].reason, JobReason::Heal);
    }

    #[test]
    fn gaps_are_planned_after_the_freshness_job_oldest_first() {
        // freshness must never queue behind history — today's bars come first.
        let src = source("lookback_ms = 1000");
        let gaps = [(100, 200), (400, 500), (900, 950)];
        let jobs = plan_pass(&src, "BTCUSDT", &gaps, NOW, 0, 0);
        assert_eq!(jobs.len(), 4);
        assert_eq!(jobs[0].reason, JobReason::Fresh);
        assert_eq!((jobs[0].start_ms, jobs[0].end_ms), (NOW - 1_000, NOW));
        let healed: Vec<(i64, i64)> = jobs[1..].iter().map(|j| (j.start_ms, j.end_ms)).collect();
        assert_eq!(healed, vec![(100, 200), (400, 500), (900, 950)]);
        assert!(jobs[1..].iter().all(|j| j.reason == JobReason::Heal));
        assert!(jobs.iter().all(|j| j.symbol == "BTCUSDT" && j.interval == "1m"));
    }

    #[test]
    fn heal_work_is_bounded_per_pass() {
        let src = source("lookback_ms = 1000\nmax_heal_jobs = 2");
        let gaps = [(1, 2), (3, 4), (5, 6), (7, 8)];
        let jobs = plan_pass(&src, "BTCUSDT", &gaps, NOW, 0, 0);
        assert_eq!(jobs.len(), 3, "1 fresh + max_heal_jobs=2");
        let healed: Vec<(i64, i64)> = jobs[1..].iter().map(|j| (j.start_ms, j.end_ms)).collect();
        assert_eq!(healed, vec![(1, 2), (3, 4)], "oldest gaps first at cursor 0");
    }

    #[test]
    fn the_heal_cursor_rotates_so_a_permanently_empty_hole_cannot_block_the_rest() {
        // A hole the venue has no data for is never closed (a 0-row fetch never records a commit
        // key), so without rotation the same oldest N holes would be re-attempted forever and hole
        // N+1 would be attempted zero times for the life of the process.
        let gaps = [(1, 2), (3, 4), (5, 6), (7, 8)];
        let taken = |cursor: usize| -> Vec<(i64, i64)> {
            plan_heal_jobs("BTCUSDT", "1m", &gaps, 2, cursor)
                .iter()
                .map(|j| (j.start_ms, j.end_ms))
                .collect()
        };
        assert_eq!(taken(0), vec![(1, 2), (3, 4)]);
        assert_eq!(taken(2), vec![(5, 6), (7, 8)], "the next pass reaches the later holes");
        assert_eq!(taken(4), vec![(1, 2), (3, 4)], "and wraps back around");
        assert_eq!(taken(3), vec![(7, 8), (1, 2)], "a wrap mid-window is fine");
        assert_eq!(taken(usize::MAX), taken(usize::MAX % 4), "any cursor is reduced modulo");
    }

    #[test]
    fn a_wide_gap_is_chunked_so_max_heal_jobs_bounds_work_not_range_count() {
        // `find_gaps` collapses consecutive missing days into ONE range, so a 30-day hole is a
        // single range. Un-chunked that is one job of ~43k 1m bars (~43 paged requests) that blocks
        // the pass — and `stop()` with it.
        let day = 86_400_000_i64;
        let chunks = chunked_gaps(&[(0, 30 * day - 1)]);
        assert_eq!(chunks.len(), 5, "30 days at 7 days per chunk");
        assert_eq!(chunks[0], (0, 7 * day - 1));
        assert_eq!(chunks[4], (28 * day, 30 * day - 1));
        for w in chunks.windows(2) {
            assert_eq!(w[1].0, w[0].1 + 1, "chunks are contiguous and non-overlapping");
        }
        let jobs = plan_heal_jobs("BTCUSDT", "1m", &[(0, 30 * day - 1)], 2, 0);
        assert_eq!(jobs.len(), 2, "the cap counts chunks, not ranges");
        assert_eq!((jobs[0].start_ms, jobs[0].end_ms), (0, 7 * day - 1));
        assert_eq!((jobs[1].start_ms, jobs[1].end_ms), (7 * day, 14 * day - 1));
        // a range at or under one chunk is passed through verbatim
        assert_eq!(chunked_gaps(&[(5, 5)]), vec![(5, 5)]);
        assert_eq!(chunked_gaps(&[(0, HEAL_CHUNK_MS - 1)]), vec![(0, HEAL_CHUNK_MS - 1)]);
    }

    #[test]
    fn max_heal_jobs_zero_plans_no_heal_work() {
        let src = source("lookback_ms = 1000\nmax_heal_jobs = 0");
        let jobs = plan_pass(&src, "BTCUSDT", &[(1, 2), (3, 4)], NOW, 0, 0);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].reason, JobReason::Fresh);
    }

    #[test]
    fn heal_off_plans_no_heal_work_even_with_gaps() {
        // THE OPT-OUT PIN: `heal = false` makes the supervisor a pure freshness poller — the same
        // single anchored window a cron'd one-shot backfill would fetch.
        let src = source("lookback_ms = 1000\nheal = false");
        let jobs = plan_pass(&src, "BTCUSDT", &[(1, 2), (3, 4)], NOW, 0, 7);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].reason, JobReason::Fresh);
    }

    #[test]
    fn a_degenerate_gap_range_is_skipped_not_dispatched() {
        assert_eq!(plan_heal_jobs("BTCUSDT", "1m", &[(500, 100)], 8, 0), Vec::new());
        let jobs = plan_heal_jobs("BTCUSDT", "1m", &[(500, 100), (10, 20)], 8, 0);
        assert_eq!(jobs.len(), 1);
        assert_eq!((jobs[0].start_ms, jobs[0].end_ms), (10, 20));
        // a single-instant range is legitimate (one missing day collapses to a real span upstream)
        assert_eq!(plan_heal_jobs("BTCUSDT", "1m", &[(7, 7)], 8, 0).len(), 1);
    }

    #[test]
    fn no_chunk_is_ever_planned_twice_in_one_pass() {
        // `max_heal_jobs` above the chunk count must not make the rotation repeat itself.
        let jobs = plan_heal_jobs("BTCUSDT", "1m", &[(1, 2), (3, 4)], 8, 0);
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].start_ms, 1);
        assert_eq!(jobs[1].start_ms, 3);
    }

    #[test]
    fn plan_heal_jobs_is_empty_for_an_empty_gap_set() {
        assert_eq!(plan_heal_jobs("BTCUSDT", "1m", &[], 8, 0), Vec::new());
        // a non-zero cursor over an empty set must not divide by zero
        assert_eq!(plan_heal_jobs("BTCUSDT", "1m", &[], 8, 41), Vec::new());
        assert_eq!(chunked_gaps(&[]), Vec::new());
    }
}
