//! Loss reporting: the dropped-row ledger, the per-window `DropReport` and the `LossMeter`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::SeriesDrops;
use crate::store::series::SeriesId;

/// Spell a [`SeriesId`] the way [`RecorderHandle::liveness`] keys ITS map — `"kind/venue/symbol"` —
/// so "which series is gapping" and "which series is still arriving" join on one string rather than
/// on two conventions that have to be kept in step by hand.
fn series_label(id: &SeriesId) -> String {
    format!("{}/{}/{}", id.kind, id.venue, id.label())
}

/// One periodic statement of what the recorder LOST since the previous statement — the observable
/// that turns a silently-gapped tape into something an operator (or a pager) sees.
///
/// Both a `*_delta` and a `*_total` are carried on purpose: the DELTA is what deserves an alert (it
/// is the new damage, and it is zero for a healthy recorder), while the TOTAL is the running tally
/// for context in the message. A report is only ever produced when at least one delta is non-zero,
/// so receiving one always means rows were lost.
///
/// NOT `Copy` since gaining [`dropped_series`](Self::dropped_series) — a scalar report cannot name
/// anything, which was the whole defect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropReport {
    /// Rows the sink never enqueued since the last report (channel full, or writer already gone).
    pub dropped_delta: u64,
    /// Cumulative rows never enqueued, for this recorder's whole life.
    pub dropped_total: u64,
    /// WHICH series those `dropped_delta` rows belonged to, worst-hit first (ties broken by the key,
    /// so the order is stable run to run). Series with a zero delta are absent, so the list is
    /// empty exactly when `dropped_delta` is zero.
    ///
    /// The deltas here sum to `dropped_delta`, with ONE stated exception: a message that names no
    /// series at all (only `Msg::Shutdown`, which is sent BLOCKING and so never reaches the drop
    /// path) counts in the scalar and nowhere else. The scalar is therefore never smaller than what
    /// the names account for — a lost row can never hide behind a missing attribution.
    ///
    /// There is deliberately no `discarded` twin: a discarded batch is charged inside
    /// `flush_buf_with`, which logs its own `venue`/`symbol` at the point of failure.
    pub dropped_series: Vec<SeriesDrops>,
    /// Buffered rows thrown away since the last report by a permanently-failed flush.
    pub discarded_delta: u64,
    /// Cumulative buffered rows thrown away by permanently-failed flushes.
    pub discarded_total: u64,
    /// Wall-clock span these deltas cover (time since the previous report, or since spawn).
    pub window: Duration,
    /// `true` for the closing report emitted as the writer thread tears down; `false` for a
    /// periodic one. A final report can cover a shorter window than `drop_report_every`.
    pub final_report: bool,
}

impl DropReport {
    /// Total rows lost in this window, whatever the reason.
    pub fn lost_delta(&self) -> u64 {
        self.dropped_delta + self.discarded_delta
    }

    /// Total rows lost over the recorder's whole life, whatever the reason.
    pub fn lost_total(&self) -> u64 {
        self.dropped_total + self.discarded_total
    }

    /// [`dropped_series`](Self::dropped_series) rendered for a human: `"kind/venue/symbol=N"`,
    /// worst first, comma-joined — what the `warn!` line carries in place of a bare scalar.
    ///
    /// `"none"` when this window dropped nothing at the channel (a report whose whole loss was a
    /// failed flush), so the field is never blank.
    pub fn dropped_series_summary(&self) -> String {
        render_drop_tally(self.dropped_series.iter().map(|s| (&s.series, s.delta)))
    }
}

/// How many series one rendered tally NAMES before summarising the rest as `"(+N more)"`.
///
/// A cap is needed because the alternative is a log line whose length is the SUBSCRIPTION WIDTH —
/// a recorder carrying a wide Polymarket family would emit one line per report long enough that
/// nobody reads any of it. The tail keeps the line honest: it states how much was elided, so a
/// truncated list never reads as a complete one, and [`RecorderHandle::dropped_by_series`] hands
/// over the whole tally for anything that wants to enumerate rather than read.
const REPORTED_SERIES: usize = 5;

/// The ONE renderer behind [`DropReport::dropped_series_summary`] and
/// [`RecorderHandle::dropped_summary`], so a series can never be spelled two ways depending on
/// which end asked.
///
/// Sorts here rather than trusting the caller: a `BTreeMap` snapshot arrives in KEY order, and the
/// order worth reading is by damage.
pub(crate) fn render_drop_tally<'a>(entries: impl Iterator<Item = (&'a SeriesId, u64)>) -> String {
    let mut rows: Vec<(&SeriesId, u64)> = entries.filter(|(_, n)| *n > 0).collect();
    if rows.is_empty() {
        return "none".to_string();
    }
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let elided = rows.len().saturating_sub(REPORTED_SERIES);
    let mut named: Vec<String> = Vec::new();
    for &(series, n) in rows.iter().take(REPORTED_SERIES) {
        named.push(format!("{}={n}", series_label(series)));
    }
    let mut out = named.join(", ");
    if elided > 0 {
        out.push_str(&format!(" (+{elided} more)"));
    }
    out
}

/// Notified once per non-empty [`DropReport`], from the recorder's WRITER thread.
///
/// Same fire-and-forget contract as `vike_alerting::AlertSink::deliver`, which is the intended
/// destination: it must not block for long and must not panic — it runs on the thread that owns
/// every store append, so time spent here is time the buffers are not being drained. It names no
/// alerting type by design (see the module doc for the dependency-cycle reason); a binary converts
/// the report into whatever it pages with.
pub type DropObserver = Arc<dyn Fn(&DropReport) + Send + Sync>;

/// The dropped-row ledger: how many rows never made it onto the writer's queue, and WHICH SERIES
/// they belonged to.
///
/// ⚠ **The split between the two members is a hot-path rule, not tidiness.** Neither is touched on
/// the SUCCESS path — an accepted row costs one `try_send` and nothing else (see
/// [`RecorderSink::send`]). Both are touched only on the failure branch, where the row is already
/// lost. `total` stays an atomic so [`RecorderHandle::dropped`] remains a lock-free scalar read;
/// the per-series tally cannot be an atomic at all, so it is a `Mutex`, and taking one is
/// affordable exactly here: a `try_send` that FAILED has already been through the channel's own
/// internal lock, so this adds no new class of contention to a path that is by definition not
/// keeping up.
pub(crate) struct DropTally {
    /// Cumulative rows never enqueued, across every series.
    total: AtomicU64,
    /// Cumulative rows never enqueued, per series. `BTreeMap` so a snapshot is already ordered and
    /// a report reads the same way twice.
    ///
    /// It grows only with series that have ACTUALLY dropped a row — a strictly smaller set than
    /// [`LiveMap`]'s, which grows with every series that ever RECEIVED one — so a rotating symbol
    /// set (a Polymarket family) introduces no growth shape this crate did not already carry.
    by_series: Mutex<BTreeMap<SeriesId, u64>>,
}

impl DropTally {
    pub(crate) fn new() -> Self {
        Self { total: AtomicU64::new(0), by_series: Mutex::new(BTreeMap::new()) }
    }

    /// Count one lost row.
    ///
    /// `series` is `None` only for a message that names none — `Msg::Shutdown`, which is sent with
    /// a BLOCKING `send` and so cannot reach here. The total counts it either way: an unattributable
    /// loss must still be a loss, never a row that vanishes because nobody could label it.
    pub(crate) fn record(&self, series: Option<SeriesId>) {
        let mut by = self.by_series.lock().unwrap();
        if let Some(series) = series {
            *by.entry(series).or_insert(0) += 1;
        }
        // Bumped while the map lock is HELD, so `snapshot` (which reads both under it) can never
        // see a total the per-series rows do not yet account for — which is what lets a report
        // state the two side by side without them contradicting each other.
        self.total.fetch_add(1, Ordering::Relaxed);
    }

    /// The scalar alone, lock-free. May be momentarily AHEAD of the per-series map (a concurrent
    /// `record` bumps it last); every reader of both together goes through [`Self::snapshot`].
    pub(crate) fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    /// Both counters as of ONE instant — see [`Self::record`] for why the lock spans them.
    pub(crate) fn snapshot(&self) -> (u64, BTreeMap<SeriesId, u64>) {
        let by = self.by_series.lock().unwrap();
        (self.total.load(Ordering::Relaxed), by.clone())
    }
}

/// The writer thread's loss-reporting state: the two shared counters, the optional observer, and
/// the watermark of what has ALREADY been reported (which is what makes each report a delta rather
/// than a re-statement of the running total).
///
/// Lives on the writer thread only. The sink side never touches it — `RecorderSink::send` still
/// does exactly one `try_send` plus, on failure, one relaxed `fetch_add`, because that path runs on
/// venue market-data pump threads (and, for `record_equity`, on the core's equity sampler), so it
/// must not grow work.
pub(crate) struct LossMeter {
    dropped: Arc<DropTally>,
    pub(crate) discarded: Arc<AtomicU64>,
    observer: Option<DropObserver>,
    every: Duration,
    last_report: Instant,
    /// The `dropped`/`discarded` totals as of the previous report — subtracted to get the delta.
    seen_dropped: u64,
    seen_discarded: u64,
    /// The same watermark, PER SERIES: what each series' cumulative drop count was at the previous
    /// report. Without it the per-series list would re-state a series' running total every window,
    /// which is the exact bug the scalar watermarks exist to avoid.
    seen_by_series: BTreeMap<SeriesId, u64>,
}

impl LossMeter {
    pub(crate) fn new(
        dropped: Arc<DropTally>,
        discarded: Arc<AtomicU64>,
        observer: Option<DropObserver>,
        every: Duration,
    ) -> Self {
        Self {
            dropped,
            discarded,
            observer,
            every,
            last_report: Instant::now(),
            seen_dropped: 0,
            seen_discarded: 0,
            seen_by_series: BTreeMap::new(),
        }
    }

    /// Report if at least `every` has elapsed since the last check. Called once per writer-loop
    /// iteration, so the real cadence is bounded below by the loop's own wake interval.
    pub(crate) fn report_if_due(&mut self) {
        if self.last_report.elapsed() >= self.every {
            self.emit(false);
        }
    }

    /// The closing report, emitted at teardown regardless of cadence so a burst in the final
    /// window is never swallowed by the shutdown.
    pub(crate) fn report_final(&mut self) {
        self.emit(true);
    }

    /// Compare both counters against the reported watermark, advance the watermark, and — ONLY if
    /// something was actually lost — warn and notify the observer.
    ///
    /// The watermark and the cadence clock advance even on an all-zero window: that is what keeps
    /// consecutive reports disjoint (their deltas sum to the total) and the cadence regular.
    fn emit(&mut self, final_report: bool) {
        // ONE snapshot for both halves (see `DropTally::snapshot`): the scalar and the names must
        // describe the same instant, or a report could name more rows than it says were lost.
        let (dropped_total, by_series) = self.dropped.snapshot();
        let discarded_total = self.discarded.load(Ordering::Relaxed);

        // The per-series deltas, on exactly the watermark discipline the scalars use: a series that
        // lost nothing THIS window is left out entirely rather than listed with a zero, so the list
        // is as silent as the report itself.
        let mut dropped_series: Vec<SeriesDrops> = Vec::new();
        for (series, total) in &by_series {
            let seen = self.seen_by_series.get(series).copied().unwrap_or(0);
            let delta = total.saturating_sub(seen);
            if delta > 0 {
                let series = series.clone();
                dropped_series.push(SeriesDrops { series, delta, total: *total });
            }
        }
        // Worst first; ties broken by the key so two runs of one incident read the same.
        dropped_series.sort_by_key(|s| (std::cmp::Reverse(s.delta), s.series.clone()));

        let report = DropReport {
            dropped_delta: dropped_total.saturating_sub(self.seen_dropped),
            dropped_total,
            dropped_series,
            discarded_delta: discarded_total.saturating_sub(self.seen_discarded),
            discarded_total,
            window: self.last_report.elapsed(),
            final_report,
        };
        self.seen_dropped = dropped_total;
        self.seen_discarded = discarded_total;
        self.seen_by_series = by_series;
        self.last_report = Instant::now();

        // A healthy recorder says NOTHING — no log line, no observer call. This is what makes the
        // first warn meaningful instead of background noise.
        if report.lost_delta() == 0 {
            return;
        }

        // Bound the named set BEFORE the macro: the field and the message must say the same thing,
        // and a line whose length tracks the subscription width is one nobody reads.
        let series = report.dropped_series_summary();
        tracing::warn!(
            dropped_delta = report.dropped_delta,
            dropped_total = report.dropped_total,
            dropped_series = %series,
            discarded_delta = report.discarded_delta,
            discarded_total = report.discarded_total,
            window_ms = report.window.as_millis() as u64,
            final_report,
            "RecorderSink: lost {} rows in the last {}ms ({} not enqueued [{}], {} discarded by a \
             failed flush) — the recorded tape has a gap; {} lost in total",
            report.lost_delta(),
            report.window.as_millis(),
            report.dropped_delta,
            series,
            report.discarded_delta,
            report.lost_total(),
        );

        if let Some(observer) = &self.observer {
            observer(&report);
        }
    }
}
