//! `RecorderSink` — the buffered live-tick writer actor onto [`crate::DataFusionHist`]
//! (tick-producer T3, `docs/superpowers/plans/2026-07-08-polymarket-tick-producer.md`).
//!
//! A [`crate::live::LiveDataSink`] implementation that persists live quotes+trades into the
//! DataFusion+Parquet hist store. The sink end (`RecorderSink::quote`/`trade`, called from a
//! venue feed's own worker thread) is **never allowed to block**: it only ever `try_send`s a row
//! onto an `mpsc::sync_channel`; a full or disconnected channel bumps a dropped-row counter
//! instead of blocking or panicking. ALL store I/O — every `append_quotes`/`append_trades` call —
//! happens on ONE dedicated writer thread that owns the `Arc<DataFusionHist>`, so a slow flush
//! never stalls a feed.
//!
//! `RecorderSink::book` (the FOLDED-state verb) is a **documented no-op** (spec decision T3):
//! `L2Book` is not `Serialize` and carries no delta/replay information, so folded snapshots are
//! never recorded here. Same for the bar verbs (`seed_bars`/`close_bar`/`forming_bar`/
//! `mark_tick`) — this sink covers ticks (quotes+trades) and RAW book events only.
//!
//! **Book recording (book-recording plan, task 6).** The recordable book lane is
//! `RecorderSink::book_update` — one RAW `BookUpdate` (delta / snapshot-anchor) per call, buffered
//! and flushed exactly like quotes/trades into the `kind=book` store series
//! (`store.append_book_updates`). `RecorderSink::stream_status` additionally records §B
//! stream-health transitions (`GapStart`/`Stale`/`Live`) for the two L2 lanes — `"book"` into
//! `kind=book` and `"depth"` into `kind=depth` (quote/trade gap provenance is still a noted
//! follow-up) — as single-row, zero-level `BookUpdate`s whose `kind` is one of the three status
//! kinds (`GapStart`/`Stale`/`LiveResume`) rather than `Delta`/`Snapshot` — replay treats them as
//! markers, not book content. ⚠ `"depth"` was excluded until 2026-09-10 and its exclusion is why a
//! forty-day binance perp reconnect loop left no trace in the store; see that method's own doc.
//!
//! **Equity lane (portfolio-observer PR-3, task 3).** `RecorderSink::record_equity` is the
//! producer for [`vike_model::EquitySample`] rows — built on the exact same
//! `try_send`/dropped-counter/buffered-flush machinery as `quote`/`trade`/`book_update`, but NOT
//! part of the [`LiveDataSink`] trait: equity samples come from the vike-core equity sampler, not
//! a venue feed. Keyed by `(venue, symbol)` where `venue` is the fixed `"portfolio"` partition
//! namespace and `symbol` carries the per-exchange venue name or the cross-venue `"TOTAL"`
//! rollup, mirroring `HistStore::append_equity`/`scan_equity`. Equity sampling is low-rate
//! (~1/sec), so the existing [`RecorderConfig`] defaults apply unchanged — no new config.
//!
//! **Buffering.** The writer keeps one `Vec` buffer per `(venue, symbol)` for quotes and another
//! for trades. A buffer flushes (one `append_quotes`/`append_trades` call, keyed for idempotency)
//! when any of: `rows.len() >= max_rows`, the buffer's age `>= max_age`, an incoming row's UTC
//! date differs from the buffer's (rollover — a buffer never straddles a UTC day, matching the
//! store's `date=` partitioning), or on shutdown (flush every remaining buffer). The writer polls
//! with `recv_timeout` so age-based flushes fire even on a quiet feed with no new messages; the
//! wait is capped at `min(max_age, 1s)` so a short `max_age` (e.g. in tests) is still honored
//! promptly while a long one (the 30s default) still gets checked at least once a second.
//!
//! Commit key: `"live-{venue}-{symbol}-{kind}-{first_ts}-{last_ts}-{flush_seq}"` (`kind` =
//! `"quote"`/`"trade"`, `first_ts`/`last_ts` the buffer's first/last row timestamps, `flush_seq` a
//! per-writer-thread monotonic counter bumped on every flush). The `flush_seq` suffix is
//! load-bearing, not decoration: two flushes of the same series CAN share `(first_ts, last_ts)`
//! (e.g. more than `max_rows` rows land in the same millisecond, forcing back-to-back max-rows
//! flushes with an identical first/last timestamp), and the store treats a repeated `commit_key`
//! as already-committed — a silent no-op (see `commit_rows` in `datafusion_hist.rs`). Without the
//! counter, the second batch would vanish silently. `flush_seq` is drawn ONCE per `flush_buf` call
//! and reused by that call's retry (below), so a retry is idempotent by construction: if the failed
//! attempt had in fact committed, the retry sees its own key in the manifest and returns `Ok(0)`
//! rather than double-writing the batch.
//!
//! **Loss reporting — every lost row is announced (`DropReport`).** Two DIFFERENT losses can happen
//! here, and both used to be effectively silent:
//!
//! 1. **`dropped`** — a row the sink never enqueued, because the channel was full (the writer is
//!    behind) or the writer thread had already exited. `RecorderSink::send` counts these, and
//!    counts them **per series** — a [`SeriesId`], the same identity `list_series` hands back — so a
//!    report NAMES the gapped tape instead of handing an operator one scalar covering every series
//!    sharing the queue.
//! 2. **`discarded`** — rows already buffered on the writer thread that a failed
//!    `append_*` threw away (see `flush_buf`).
//!
//! **Why `dropped` is keyed and `discarded` is not.** ONE bounded queue carries every series, so a
//! full queue fails EVERY producer's `try_send`. That is not starvation and nobody is being
//! preferred: a low-rate producer simply gets far fewer attempts at a freed slot, so it loses a
//! much larger SHARE of its own rows. On the CI box a Polymarket L2 stream and a Binance tape share this
//! queue, and under a writer stall the quiet one is the tape that thins out. Two existing
//! mechanisms almost see it and each misses by the same inch: `flush_buf_with` logs `venue`/`symbol`
//! but only on a PERMANENT FLUSH FAILURE, and [`RecorderHandle::liveness`] (with
//! `vike_recorder::liveness::SilenceWatch` above it) names a series that stops ENTIRELY — a series
//! still receiving SOME rows looks alive to both. PARTIAL loss is the residual between them, and the
//! key is what closes it. `discarded` needs no key: it is charged inside `flush_buf_with`, which
//! already knows and logs the series it was flushing.
//!
//! ⚠ **The key costs the SUCCESS path nothing.** `try_send` hands the message BACK on failure (both
//! `TrySendError` variants carry it), so `RecorderSink::send` recovers the venue and symbol from the
//! REJECTED message on the failure branch rather than cloning them on the way in. An accepted row
//! still costs exactly one `try_send` and not one instruction more.
//!
//! Counting is not observing. `RecorderHandle::dropped()` had exactly TWO call sites in the whole
//! workspace, both `#[cfg(test)]` in this file — no binary read it — so a live recorder could gap
//! its tape indefinitely with nothing to see. That is the failure mode `vike-alerting` was split out
//! of `vike-ops` for (the latency box crashed and ~6h of recorded Polymarket L2 tape was lost UNNOTICED), with
//! the metric that would have caught it left unread.
//!
//! So the writer thread now REPORTS. On a wall-clock cadence (`RecorderConfig::drop_report_every`,
//! default 60s, plus one closing report at teardown) it compares each counter against what it last
//! reported and, **only when a delta is non-zero**, emits one `tracing::warn!` and calls the optional
//! [`DropObserver`]. Reporting the DELTA rather than the cumulative total is the point: a cumulative
//! total is non-zero forever after the first drop, so warning on it would either spam one line per
//! cadence for the rest of the process's life or be tuned out entirely. A healthy recorder emits
//! nothing at all, which is what makes the first warn meaningful.
//!
//! **Why the observer takes a plain struct instead of an `AlertSink`.** Paging on this belongs in
//! `vike-alerting` — but `vike-data` CANNOT depend on it: `vike-alerting`'s `core` feature depends on
//! `vike-core`, and `vike-core` depends on `vike-data`, so the edge is a package cycle cargo refuses
//! to build (and even without the cycle it would drag `ureq`+rustls into the bottom-layer crate that
//! `vike-datahub-client`/`vike-cli` stay light by depending on). [`DropObserver`] therefore names no
//! alerting type: it hands out a plain [`DropReport`], and a BINARY bridges it — `vike_alerting`'s
//! `FiredAlert` lives in that crate's vike-FREE default half precisely so a consumer can build and
//! deliver one without linking the trading core. That keeps the workspace rule ("libraries take
//! configuration as parameters; only binaries wire") intact. Wiring the observer in `vike-app`'s and
//! `vike-tradehub`'s `main.rs` is the follow-up; `spawn` (no observer) is byte-identical to today,
//! so no existing call site changes.
//!
//! **Flush retry.** `flush_buf` used to discard its whole buffer (up to `max_rows` = 5,000 rows) on
//! the FIRST `append_*` error. It now retries exactly once after a short delay. Deliberately narrow,
//! because most of the retry budget already lives one layer down: `SeriesLock::acquire` itself spins
//! ~4s (2000 × 2ms) on a CONTENDED lock, so a retry adds nothing there. (It used to be worse: a
//! killed writer left a lock file no retry could ever survive, because the lock WAS that file's
//! existence. The lock is now the OS advisory lock on it, which the kernel releases when the holder
//! dies — see `SeriesLock`.) The retry therefore targets the case the store does NOT already handle: a
//! transient I/O error that fails FAST. A first attempt that took `SLOW_ATTEMPT` or longer is NOT
//! retried at all — it has already burned the lock spin, and retrying would only double the
//! single writer thread's stall and so double the `dropped` rows piling up behind it. After the last
//! attempt the rows ARE discarded (there is nowhere to put them) — but now they are counted into
//! `discarded` and surface through the same `DropReport`, so a permanently-wedged store reads as a
//! rising loss count instead of a scroll of individual warns.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use vike_model::{
    Bar, BookUpdate, BookUpdateKind, EquitySample, L2Book, Level, QuoteTick, TradeTick,
};

use crate::datafusion_hist::DataFusionHist;
use crate::datafusion_hist::GroupResolver;
use crate::hist::{DataError, HistStore};
use crate::live::{LiveDataSink, StreamStatus};
use crate::series::SeriesId;
use vike_model::time::epoch_ms_to_utc_date;

/// Flush-policy knobs for [`RecorderSink::spawn`].
/// NOT `Copy` since gaining `grouping` — a `GroupResolver` is an `Arc<dyn Fn>`.
#[derive(Clone)]
pub struct RecorderConfig {
    /// Flush a `(venue, symbol)` buffer once it holds this many rows.
    pub max_rows: usize,
    /// Flush a buffer once its oldest row has sat unflushed this long.
    pub max_age: Duration,
    /// Bound on the sink-to-writer channel; `try_send` fails (row dropped, counted) past this.
    pub channel_cap: usize,
    /// How often the writer thread checks its loss counters and reports a non-zero DELTA (one
    /// `warn!` + one [`DropObserver`] call). See the module doc's "Loss reporting" section.
    ///
    /// There is deliberately no "off" value: silence about lost rows is the bug this exists to fix,
    /// and a healthy recorder already reports nothing (an all-zero delta emits nothing), so the
    /// worst case for a broken one is one line per this interval.
    ///
    /// ⚠ **`max(drop_report_every, min(max_age, 1s))` is a LOWER bound on the cadence, not the
    /// cadence.** The check rides the writer's existing wake-up (`writer_loop`), so it cannot
    /// fire more often than the loop wakes — but it also cannot fire while the loop is blocked
    /// inside an `append_*`, and a store commit costs ~30-39 ms even on an idle box. The real
    /// cadence is therefore `max(drop_report_every, min(max_age, 1s)) + however long the writer is
    /// stalled in the store`, and the stall term is unbounded exactly when it matters: a writer
    /// slow enough to make the sink drop rows is a writer slow to announce it. MEASURED on the
    /// the CI box CI box (`chatty(1)`: 50 ms cadence, 20 ms `max_age`, one burst of ~19,000 dropped
    /// rows) — first report after the burst at ~80 ms idle, p90 176 ms and max 350 ms under a
    /// 4-way parallel test load, max 1,305 ms under an 8-way one. The delta is never LOST by this
    /// (it accumulates and the next report carries it; teardown always emits a closing one), only
    /// delayed — which is why a fixed sleep is the wrong way for a test to wait for one, see
    /// `a_drop_burst_is_reported_once_per_delta_then_falls_silent`.
    pub drop_report_every: Duration,
    /// `Some` ⇒ flush into GROUPED series: buffers whose `(venue, symbol)` resolves to the same
    /// group merge into ONE commit instead of one per symbol.
    ///
    /// This is the live half of what `BulkIngestSession::bulk_session_grouped` does for backfills,
    /// and it exists for the same reason: a customer recording a wide subscription (a Polymarket
    /// family, or every USDT perp) otherwise pays the store's ~30-39 ms per-commit fixed floor once
    /// PER SYMBOL, on ONE writer thread. Grouping is what turns that into once per family.
    ///
    /// ⚠ Routing to a group WITHOUT merging would be strictly worse than not grouping: every
    /// per-symbol buffer would still be its own commit, now all contending on ONE `SeriesLock`
    /// instead of N independent ones. The merge is the feature; the path is just where it lands.
    ///
    /// Returning `None` for a series keeps it per-symbol, so a customer can migrate one family at a
    /// time. `None` overall is exactly today's behaviour, byte for byte.
    pub grouping: Option<GroupResolver>,
}

impl Default for RecorderConfig {
    fn default() -> Self {
        Self {
            max_rows: 5_000,
            max_age: Duration::from_secs(30),
            channel_cap: 65_536,
            drop_report_every: Duration::from_secs(60),
            grouping: None,
        }
    }
}

/// Hand-written because `GroupResolver` is an `Arc<dyn Fn>`, which has no `Debug`.
impl std::fmt::Debug for RecorderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecorderConfig")
            .field("max_rows", &self.max_rows)
            .field("max_age", &self.max_age)
            .field("channel_cap", &self.channel_cap)
            .field("drop_report_every", &self.drop_report_every)
            .field("grouping", &self.grouping.as_ref().map(|_| "<resolver>"))
            .finish()
    }
}

/// Delay between a failed `append_*` and its ONE retry (see the module doc's "Flush retry").
const FLUSH_RETRY_DELAY: Duration = Duration::from_millis(100);

/// A first `append_*` attempt that took at least this long is NOT retried: it has already spent the
/// store's own internal retry budget (`SeriesLock::acquire` spins ~4s on a contended lock), so a
/// second attempt would mostly just double this single writer thread's stall.
const SLOW_ATTEMPT: Duration = Duration::from_secs(1);

/// Spell a [`SeriesId`] the way [`RecorderHandle::liveness`] keys ITS map — `"kind/venue/symbol"` —
/// so "which series is gapping" and "which series is still arriving" join on one string rather than
/// on two conventions that have to be kept in step by hand.
fn series_label(id: &SeriesId) -> String {
    format!("{}/{}/{}", id.kind, id.venue, id.label())
}

/// One series' share of a [`DropReport`]'s `dropped` axis — same delta/total pairing, and for the
/// same reason (see [`DropReport`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesDrops {
    /// The series that lost the rows.
    ///
    /// Always a `per_symbol` id with `interval: None` and `group: None`, and that is a statement
    /// about WHERE the loss happened rather than a shortcut. A row is dropped at the CHANNEL, one
    /// layer before the writer thread decides whether its buffer flushes grouped (`flush_batch` is
    /// what consults [`RecorderConfig::grouping`], and it never sees a row that was never
    /// enqueued); and this sink records tick kinds only, which sub-partition by no interval. It is
    /// the same identity `DataFusionHist::list_series` returns for a per-symbol tick series, so a
    /// report joins against an inventory listing with no translation step.
    pub series: SeriesId,
    /// Rows of THIS series never enqueued since the last report. Always non-zero: a series with a
    /// zero delta is left out of the report entirely rather than listed as fine.
    pub delta: u64,
    /// Cumulative rows of this series never enqueued, for the recorder's whole life.
    pub total: u64,
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
fn render_drop_tally<'a>(entries: impl Iterator<Item = (&'a SeriesId, u64)>) -> String {
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
struct DropTally {
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
    fn new() -> Self {
        Self { total: AtomicU64::new(0), by_series: Mutex::new(BTreeMap::new()) }
    }

    /// Count one lost row.
    ///
    /// `series` is `None` only for a message that names none — `Msg::Shutdown`, which is sent with
    /// a BLOCKING `send` and so cannot reach here. The total counts it either way: an unattributable
    /// loss must still be a loss, never a row that vanishes because nobody could label it.
    fn record(&self, series: Option<SeriesId>) {
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
    fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    /// Both counters as of ONE instant — see [`Self::record`] for why the lock spans them.
    fn snapshot(&self) -> (u64, BTreeMap<SeriesId, u64>) {
        let by = self.by_series.lock().unwrap();
        (self.total.load(Ordering::Relaxed), by.clone())
    }
}

/// One buffered row, tagged with the `(venue, symbol)` it belongs to (the writer thread's queue
/// is a single ordered stream across every series — this is what lets a rollover flush and a
/// max-rows flush interleave deterministically with plain FIFO delivery).
enum Msg {
    Quote {
        venue: String,
        symbol: String,
        quote: QuoteTick,
    },
    Trade {
        venue: String,
        symbol: String,
        trade: TradeTick,
    },
    Book {
        venue: String,
        symbol: String,
        update: BookUpdate,
    },
    /// A CONFLATING L2 depth snapshot — `LiveDataSink::l2_snapshot`. Its own message and its own
    /// buffer, never folded into `Book`, because the two lanes make different promises: see
    /// `HistStore::append_depth`.
    Depth {
        venue: String,
        symbol: String,
        update: BookUpdate,
    },
    Equity {
        venue: String,
        symbol: String,
        sample: EquitySample,
    },
    /// One-shot teardown signal: flush every buffer, then the writer thread returns.
    Shutdown,
}

impl Msg {
    /// The series this message was headed for, CONSUMING it.
    ///
    /// Called from exactly one place — [`RecorderSink::send`]'s failure branch, on the message
    /// `try_send` handed BACK — which is what keeps attribution off the success path entirely: the
    /// owned `venue`/`symbol` are MOVED out of a row that is already lost, so an accepted row never
    /// builds a key at all and nothing is cloned on the way in.
    ///
    /// `None` only for [`Msg::Shutdown`], which names no series and is sent with a blocking `send`,
    /// so it never reaches that branch in the first place.
    ///
    /// The `kind` comes from the matching [`RecKind`]'s own `tag`, never a second spelling of the
    /// tag set — the same string the store partitions by and [`series_label`] renders.
    fn into_series_id(self) -> Option<SeriesId> {
        let (kind, venue, symbol) = match self {
            Msg::Quote { venue, symbol, .. } => (QUOTE.tag, venue, symbol),
            Msg::Trade { venue, symbol, .. } => (TRADE.tag, venue, symbol),
            Msg::Book { venue, symbol, .. } => (BOOK.tag, venue, symbol),
            Msg::Depth { venue, symbol, .. } => (DEPTH.tag, venue, symbol),
            Msg::Equity { venue, symbol, .. } => (EQUITY.tag, venue, symbol),
            Msg::Shutdown => return None,
        };
        // `venue`/`symbol` are MOVED in (`Into<String>` over an owned `String` is the identity), so
        // the only allocation this whole path makes is the `kind` tag's — on the failure branch,
        // for a row that is already lost.
        Some(SeriesId::per_symbol(kind, venue, symbol, None))
    }
}

/// The `LiveDataSink` end: cheap, non-blocking, `Send + Sync` (shared across feed threads via
/// `Arc`). Holds only a channel sender + the shared dropped-row counter — no store access here.
pub struct RecorderSink {
    tx: SyncSender<Msg>,
    dropped: Arc<DropTally>,
    /// Monotonic-distinct `seq` for the CONFLATING depth lane — see [`RecorderSink::l2_snapshot`].
    ///
    /// A conflated feed carries no usable sequence of its own, but `BookCodec`'s regroup uses
    /// `(seq, kind)` as FRAME IDENTITY, not merely as a contiguity check: consecutive `Snapshot`
    /// rows sharing a seq are folded into ONE event. A constant `0` therefore merges every depth
    /// frame into the first — which the codec's own `debug_assert` catches, and which is how this
    /// counter came to exist.
    depth_seq: Arc<AtomicU64>,
}

impl RecorderSink {
    /// Spawn the writer thread and return the `(sink, handle)` pair: the sink is the
    /// `Arc<dyn LiveDataSink>`-able end feeds call into; the handle is the owner-side teardown +
    /// diagnostics end (`dropped()` / `shutdown()`).
    ///
    /// Returns `Err` if the OS refuses to spawn the writer thread (final-review fix — this used to
    /// `.expect()`, panicking the whole app on a transient OS resource failure; the app's
    /// "ticks must still flow" best-effort discipline requires callers to be able to degrade to
    /// the bare non-recording sink instead of crashing).
    pub fn spawn(
        store: Arc<DataFusionHist>,
        cfg: RecorderConfig,
    ) -> std::io::Result<(Arc<RecorderSink>, RecorderHandle)> {
        Self::spawn_with_observer(store, cfg, None)
    }

    /// [`spawn`](Self::spawn) plus an optional [`DropObserver`] notified on every non-empty
    /// [`DropReport`] — the seam a binary uses to page on a gapping tape (see the module doc for
    /// why this takes a callback rather than a `vike_alerting::AlertSink`).
    ///
    /// `None` is exactly [`spawn`](Self::spawn): the periodic `warn!` still fires (loss is never
    /// silent), only the programmatic notification is absent.
    ///
    /// ⚠ **No non-test caller exists yet** — `vike-app`, `vike-tradehub`, `vike-recorder` and
    /// `poly_reparse` all call [`spawn`](Self::spawn), so a [`DropReport`] currently reaches only
    /// the `warn!` line and nothing that pages. This is the PUSH seam waiting for that wiring;
    /// [`RecorderHandle::dropped_by_series`] is the PULL twin for a binary that would rather read
    /// the tally at teardown than hold a callback, and `vike_recorder`'s stop path does exactly
    /// that.
    pub fn spawn_with_observer(
        store: Arc<DataFusionHist>,
        cfg: RecorderConfig,
        observer: Option<DropObserver>,
    ) -> std::io::Result<(Arc<RecorderSink>, RecorderHandle)> {
        let (tx, rx) = mpsc::sync_channel(cfg.channel_cap);
        let dropped = Arc::new(DropTally::new());
        let discarded = Arc::new(AtomicU64::new(0));

        let meter =
            LossMeter::new(dropped.clone(), discarded.clone(), observer, cfg.drop_report_every);
        let live: LiveMap = Arc::new(Mutex::new(HashMap::new()));
        let writer_live = live.clone();
        let join = thread::Builder::new()
            .name("vike-data-recorder".into())
            .spawn(move || writer_loop(&store, rx, &cfg, meter, writer_live))?;

        let sink = Arc::new(RecorderSink {
            tx: tx.clone(),
            dropped: dropped.clone(),
            depth_seq: Arc::new(AtomicU64::new(1)),
        });
        let handle = RecorderHandle { tx, dropped, discarded, live, join: Some(join) };
        Ok((sink, handle))
    }

    /// `try_send` a message; a full or disconnected channel counts as a dropped row rather than
    /// blocking the calling feed thread or panicking.
    ///
    /// ⚠ **The drop is ATTRIBUTED, and the attribution is free on the success path.** Both
    /// `TrySendError` variants hand the message BACK, so the `(kind, venue, symbol)` is MOVED out
    /// of the rejected message on the failure branch — never cloned on the way in, never computed
    /// for a row that was accepted. This used to read `if self.tx.try_send(msg).is_err()`, which
    /// threw that message (and with it the only copy of the venue and symbol) away: one
    /// `AtomicU64` then covered every series sharing the queue, so a gapped tape could be counted
    /// but never NAMED. See the module doc's "Why `dropped` is keyed" for why the quiet series is
    /// the one that suffers.
    fn send(&self, msg: Msg) {
        match self.tx.try_send(msg) {
            Ok(()) => {}
            Err(TrySendError::Full(msg) | TrySendError::Disconnected(msg)) => {
                self.dropped.record(msg.into_series_id());
            }
        }
    }

    /// One equity-curve sample (portfolio-observer PR-3) — same non-blocking `try_send` +
    /// dropped-counter contract as `LiveDataSink::quote`, but NOT part of the `LiveDataSink`
    /// trait: equity samples come from the vike-core equity sampler, not a venue feed. `venue` is
    /// the fixed `"portfolio"` partition namespace at the call site; `symbol` carries the
    /// per-exchange venue name or the cross-venue `"TOTAL"` rollup (mirrors
    /// `HistStore::append_equity`).
    pub fn record_equity(&self, venue: &str, symbol: &str, sample: EquitySample) {
        self.send(Msg::Equity { venue: venue.to_string(), symbol: symbol.to_string(), sample });
    }
}

impl LiveDataSink for RecorderSink {
    fn seed_bars(&self, _venue: &str, _symbol: &str, _interval: &str, _bars: Vec<Bar>) {
        // Out of scope: this sink records ticks (quotes+trades) only.
    }
    fn close_bar(&self, _venue: &str, _symbol: &str, _interval: &str, _bar: Bar) {}
    fn forming_bar(&self, _venue: &str, _symbol: &str, _interval: &str, _bar: Bar) {}
    fn mark_tick(&self, _venue: &str, _symbol: &str, _px: f64, _ts: i64) {}
    /// Explicitly no-op, not merely the inherited trait default: persisting the venue mark and
    /// candle-close series (a store `kind=mark`) is deliberately deferred, so the choice is
    /// stated here rather than read as an oversight.
    fn bar_close_tick(&self, _venue: &str, _symbol: &str, _px: f64, _ts: i64) {}

    fn quote(&self, venue: &str, symbol: &str, quote: QuoteTick) {
        self.send(Msg::Quote { venue: venue.to_string(), symbol: symbol.to_string(), quote });
    }

    fn trade(&self, venue: &str, symbol: &str, trade: TradeTick) {
        self.send(Msg::Trade { venue: venue.to_string(), symbol: symbol.to_string(), trade });
    }

    fn book(&self, _venue: &str, _symbol: &str, _book: Arc<L2Book>) {
        // Documented no-op (spec T3): this is the FOLDED-state verb — it carries only the folded
        // book, with no delta/replay information to reconstruct a chain from. The recordable book
        // lane is `book_update` (below), which persists the RAW `BookUpdate` wire events into the
        // `kind=book` series. (Dropping the `Arc` here is a refcount decrement, nothing more.)
    }

    fn book_update(&self, venue: &str, symbol: &str, update: BookUpdate) {
        self.send(Msg::Book { venue: venue.to_string(), symbol: symbol.to_string(), update });
    }

    /// Persist a CONFLATING L2 depth snapshot into the `kind=depth` series.
    ///
    /// This verb used to be the trait's default NO-OP, and that silently discarded the only L2 a
    /// depth-serving venue offers. Binance/bybit/okx declare `book: false, depth: true`
    /// (`vike_model::venue_caps`) and refuse `subscribe_book` outright — so for those venues this
    /// call is not a supplement to the book lane, it IS the L2. Dropped here, binance L2 was not
    /// obtainable in this workspace by ANY means: no venue backfill serves `book` either, and the
    /// crypto L2 archive is rights-blocked.
    ///
    /// Recorded as [`BookUpdateKind::Snapshot`], which is honest — that is exactly what a
    /// `@depth20@100ms` frame is: a full-state anchor. `seq` is `0`: this lane makes no contiguity
    /// promise, and synthesising one from the venue's update id would make gap detection fire on
    /// every frame (100 ms conflation means the id always jumps). A pure-snapshot series needs none
    /// anyway — every row resyncs by construction.
    ///
    /// ⚠ Goes to `kind=depth`, NOT `kind=book` — see [`HistStore::append_depth`]. Briefly: the book
    /// lane promises a losslessness conflated depth cannot honour, and a market-making backtest run
    /// over teleporting depth would report fills it could never have got.
    fn l2_snapshot(
        &self,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<Level>,
        asks: Vec<Level>,
        ts: i64,
    ) {
        self.send(Msg::Depth {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            update: BookUpdate {
                ts,
                // The venue stamp is all this verb carries — its signature has no receive stamp.
                local_ts: 0,
                // Monotonic-distinct, NOT the venue's update id: see `depth_seq`. Relaxed is enough —
                // this only has to be distinct, and every depth frame for a series goes through this
                // one sink.
                seq: self.depth_seq.fetch_add(1, Ordering::Relaxed),
                kind: BookUpdateKind::Snapshot,
                tick_size,
                bids,
                asks,
                symbol: symbol.to_string(),
            },
        });
    }

    /// §B disclosure → recorded stream-health markers, for the two L2 lanes that HAVE a series to
    /// mark: `book` and `depth`. Status events are single-row, zero-level `BookUpdate`s
    /// (kind ≠ Delta/Snapshot) with `seq: 0`, landing in the SAME series the lane's data rows land
    /// in — `kind=book` for `book`, `kind=depth` for `depth` (the two share `BookCodec`, so a
    /// marker roundtrips through `scan_depth` exactly as it does through `scan_book_updates`).
    ///
    /// ⚠ **`depth` was an early return here until 2026-09-10, and that was half of a forty-day
    /// blind spot.** The argument for book-only was that the book stream is "the one whose replay
    /// integrity depends on knowing where live data was missing" — which is true of `depth` MORE,
    /// not less: `book` is a lossless delta chain a reader can audit for itself, while `depth` is
    /// conflating snapshots with `seq: 0` and no contiguity promise at all, so a hole in it is
    /// invisible by construction. During those forty days `crates/bridges/binance/src/family/market_feed.rs`'s
    /// `depth_main` emitted a `GapStart` on every one of ~20,000 reconnect cycles a day and every
    /// one of them was dropped on this line, leaving a backtest reading
    /// `kind=depth/venue=binance/symbol=BTCUSDT.P` a book that teleports every 4.3 s and nothing in
    /// the store to say why.
    ///
    /// Every OTHER stream label still returns early, deliberately and for an unchanged reason: they
    /// name lanes whose rows are quotes/trades/bars, and a `BookUpdate` marker has no series there
    /// to be written into. Quote/trade gap provenance remains the noted follow-up it always was.
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        let now = vike_model::clock::now_ms();
        let (ts, kind) = match status {
            StreamStatus::GapStart { at_ts_ms } => (at_ts_ms, BookUpdateKind::GapStart),
            StreamStatus::Stale { now_ms, .. } => (now_ms, BookUpdateKind::Stale),
            StreamStatus::Live { .. } => (now, BookUpdateKind::LiveResume),
        };
        let update = BookUpdate {
            ts,
            local_ts: now,
            seq: 0,
            kind,
            tick_size: 0.0,
            bids: Vec::new(),
            asks: Vec::new(),
            symbol: String::new(),
        };
        let venue = venue.to_string();
        let symbol = symbol.to_string();
        match stream {
            "book" => self.send(Msg::Book { venue, symbol, update }),
            "depth" => self.send(Msg::Depth { venue, symbol, update }),
            _ => {}
        }
    }
}

/// The owner-side handle: diagnostics (`dropped()`) + deterministic teardown (`shutdown()` =
/// flush every remaining buffer, then join the writer thread). A `Drop` best-effort mirrors
/// `shutdown()` so a caller that merely drops the handle doesn't leak an orphaned thread — the
/// crate's established "never leak a background thread" discipline (see
/// [`crate::hist_sched::MaintenanceScheduler`]).
pub struct RecorderHandle {
    tx: SyncSender<Msg>,
    dropped: Arc<DropTally>,
    discarded: Arc<AtomicU64>,
    live: LiveMap,
    join: Option<JoinHandle<()>>,
}

impl RecorderHandle {
    /// Total rows dropped so far because the sink-to-writer channel was full (or the writer
    /// thread had already exited). Monotonic.
    ///
    /// Polling this is no longer the only way to notice: the writer thread reports every non-zero
    /// DELTA of it on its own (see the module doc's "Loss reporting"). This stays as the exact
    /// point-in-time read.
    ///
    /// ⚠ It is a SCALAR over every series sharing the one queue, which is the question an operator
    /// almost never has — [`dropped_by_series`](Self::dropped_by_series) is the one that says which
    /// tape has the hole.
    pub fn dropped(&self) -> u64 {
        self.dropped.total()
    }

    /// [`dropped`](Self::dropped) broken down by the series each row belonged to — cumulative, the
    /// same monotonic counting, keyed by [`SeriesId`] — the same identity `list_series` returns.
    ///
    /// This is the PULL half of the attribution (the push half is a [`DropReport`]'s
    /// `dropped_series`, which carries per-window deltas instead). A binary that already reads
    /// `dropped()` at teardown gets the names for one extra call.
    ///
    /// Series absent from the map have dropped nothing; a series that has dropped rows AND is still
    /// receiving them is exactly the state [`liveness`](Self::liveness) reads as healthy, which is
    /// why both exist.
    pub fn dropped_by_series(&self) -> BTreeMap<SeriesId, u64> {
        self.dropped.snapshot().1
    }

    /// [`dropped_by_series`](Self::dropped_by_series) rendered the same way the periodic `warn!`
    /// renders it — `"kind/venue/symbol=N"`, worst first, capped with a `"(+N more)"` tail, and
    /// `"none"` when nothing was dropped. One spelling, wherever the question is asked.
    pub fn dropped_summary(&self) -> String {
        let tally = self.dropped.snapshot().1;
        render_drop_tally(tally.iter().map(|(series, n)| (series, *n)))
    }

    /// Total buffered rows thrown away so far by a flush that failed its `append_*` AND its retry.
    /// Monotonic. Disjoint from [`dropped`](Self::dropped): these rows DID reach the writer thread,
    /// and were lost writing them to the store rather than on the way in.
    pub fn discarded(&self) -> u64 {
        self.discarded.load(Ordering::Relaxed)
    }

    /// Every row this recorder lost, for whatever reason — [`dropped`](Self::dropped) +
    /// [`discarded`](Self::discarded). The one number that answers "does my tape have holes?".
    pub fn lost(&self) -> u64 {
        self.dropped() + self.discarded()
    }

    /// Per-series arrival records, keyed `"{kind}/{venue}/{symbol}"` — see [`Liveness`].
    ///
    /// The counters above answer "am I LOSING rows?". This answers the question that had no
    /// answer at all: **"am I RECEIVING any?"** A series whose feed is subscribed and connected but
    /// silent never appears in `dropped`/`discarded` — there is nothing to lose — and never raises
    /// an error. Its entry here simply stops advancing, which is what a watchdog can see.
    ///
    /// A series that has NEVER received a row has no entry, deliberately: absent and stale are
    /// different states, and a caller that knows what it subscribed can tell "never started" from
    /// "started then stopped".
    pub fn liveness(&self) -> HashMap<String, Liveness> {
        self.live.lock().unwrap().clone()
    }

    /// Flush every buffered row and join the writer thread — deterministic teardown. Consumes
    /// `self`: there is nothing left to signal afterwards.
    ///
    /// The teardown signal is a blocking `send` (not `try_send`): this is the one-shot shutdown
    /// message, not a hot-path row, and the writer thread continuously drains its queue (via
    /// `recv_timeout`) so `send` can never wedge waiting for room.
    ///
    /// Ticks enqueued after the `Shutdown` message are neither flushed nor counted dropped;
    /// callers must stop feeds before shutting the recorder down (the app's documented shutdown
    /// order).
    ///
    /// The writer emits its closing [`DropReport`] before this returns. One residual, stated rather
    /// than papered over: a row dropped AFTER that report (the channel is disconnected once the
    /// writer is gone, so a still-running feed's `try_send` fails and counts) has no reporter left
    /// to announce it. It is still in [`dropped`](Self::dropped) — read it after joining if the
    /// shutdown order was not honored.
    pub fn shutdown(mut self) {
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for RecorderHandle {
    fn drop(&mut self) {
        if let Some(join) = self.join.take() {
            let _ = self.tx.send(Msg::Shutdown);
            let _ = join.join();
        }
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
struct LossMeter {
    dropped: Arc<DropTally>,
    discarded: Arc<AtomicU64>,
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
    fn new(
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
    fn report_if_due(&mut self) {
        if self.last_report.elapsed() >= self.every {
            self.emit(false);
        }
    }

    /// The closing report, emitted at teardown regardless of cadence so a burst in the final
    /// window is never swallowed by the shutdown.
    fn report_final(&mut self) {
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

/// What [`flush_buf`] writes through: the store it appends to, plus the counter a permanently
/// failed flush bumps.
///
/// Bundled into one value rather than passed as two arguments so every flush call site keeps its
/// argument count (`ingest` was already at clippy's 7-argument limit).
struct FlushCtx<'a> {
    store: &'a DataFusionHist,
    discarded: &'a AtomicU64,
    /// `Some` ⇒ merge same-group buffers into one commit (see `RecorderConfig::grouping`).
    grouping: Option<GroupResolver>,
    /// Per-series row-arrival tracking — see [`Liveness`]. Shared with [`RecorderHandle::liveness`].
    live: LiveMap,
}

/// Per-series row-arrival tracking, shared between the writer thread and [`RecorderHandle`].
///
/// **Why this exists.** A feed that is subscribed and CONNECTED but receiving nothing is invisible
/// from every other vantage point: the venue adapter's `subscribe_*` returned `Ok`, the socket is
/// ESTABLISHED in `ss`, no error is ever raised, and the store simply stops growing. A binance perp
/// subscription ran 95 minutes on the CI box in exactly that state — logging `started=2 failed=0` and
/// zero warnings — because the venue's stream had silently gone dead.
///
/// Rows ARRIVING is the one signal that means "this series is really recording", so it is tracked
/// here and the daemon compares it against a threshold each tick.
///
/// Updated in the WRITER thread (per ingested row — already off the caller's thread), never in the
/// hot [`LiveDataSink`] path.
pub type LiveMap = Arc<Mutex<HashMap<String, Liveness>>>;

/// One series' arrival record: how many rows have reached the writer, and when the last one did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Liveness {
    /// Rows ingested since this recorder started. Monotonic.
    pub rows: u64,
    /// Machine time of the most recent ingested row, epoch-ms — deliberately NOT the row's own
    /// timestamp, so replaying a historical batch cannot make a dead feed look alive.
    pub last_ms: i64,
}

/// One `(venue, symbol)` buffer of unflushed rows, all sharing the same UTC date (a date
/// mismatch on an incoming row triggers a rollover flush before the row is buffered).
struct Buffered<T> {
    rows: Vec<T>,
    date: String,
    opened_at: Instant,
}

impl<T> Buffered<T> {
    fn new(date: String) -> Self {
        Self { rows: Vec::new(), date, opened_at: Instant::now() }
    }
}

/// Per-writer-thread state: one buffer map per tick kind, keyed by `(venue, symbol)`, plus the
/// monotonic flush counter appended to every commit key (see the module doc for why).
struct WriterState {
    quotes: HashMap<(String, String), Buffered<QuoteTick>>,
    trades: HashMap<(String, String), Buffered<TradeTick>>,
    books: HashMap<(String, String), Buffered<BookUpdate>>,
    /// The CONFLATING depth lane — separate buffer, separate series. See `HistStore::append_depth`.
    depth: HashMap<(String, String), Buffered<BookUpdate>>,
    equity: HashMap<(String, String), Buffered<EquitySample>>,
    flush_seq: u64,
}

impl WriterState {
    fn new() -> Self {
        Self {
            quotes: HashMap::new(),
            trades: HashMap::new(),
            books: HashMap::new(),
            depth: HashMap::new(),
            equity: HashMap::new(),
            flush_seq: 0,
        }
    }
}

/// Draw the next flush-sequence value from `counter`, monotonic for the lifetime of the writer
/// thread. Every flush (max-rows, age, rollover, or shutdown) draws one, even across different
/// `(venue, symbol)` series and different tick kinds (every `ingest`/`flush_aged`/`flush_all_of`
/// call below threads the SAME `&mut state.flush_seq`) — this is what makes the commit key unique
/// when two flushes share `(first_ts, last_ts)`.
fn next_seq(counter: &mut u64) -> u64 {
    let seq = *counter;
    *counter += 1;
    seq
}

/// The writer thread body: drain the channel, buffer rows, flush on policy, exit on shutdown.
///
/// **Age-check cadence is decoupled from `recv`'s outcome (final-review fix).** `recv_timeout`
/// returns `Ok` immediately whenever ANY message is queued, so with N series multiplexed on this
/// one channel, a steadily-active series (always has a message ready) would starve the age check
/// forever if it only ran in the `Err(Timeout)` arm — a quiet series' buffered rows would then sit
/// unflushed (and unprotected by WAL) indefinitely. Instead `last_age_check` is tracked
/// independently: after handling EITHER arm, if it has been at least `wait` since the last scan,
/// run the aged-flush pass and reset the clock. This bounds the age scan to once per `wait`
/// regardless of how busy the channel is.
fn writer_loop(
    store: &DataFusionHist,
    rx: Receiver<Msg>,
    cfg: &RecorderConfig,
    mut meter: LossMeter,
    live: LiveMap,
) {
    let mut state = WriterState::new();
    // An independent `Arc` clone (not a borrow of `meter`) so `ctx` and the `&mut meter` reporting
    // calls below can coexist without fighting over one borrow. `&discarded` (NOT `&*discarded` —
    // that trips `clippy::explicit_auto_deref`) reaches the `&AtomicU64` field type by deref
    // coercion, which a struct-literal field is a coercion site for.
    let discarded = meter.discarded.clone();
    let ctx = FlushCtx { store, discarded: &discarded, grouping: cfg.grouping.clone(), live };
    // Wake at least once a second (so age flushes fire on a quiet feed even with a long
    // `max_age`), but no less often than `max_age` itself so a short test/tuned `max_age` is
    // still honored promptly.
    let wait = cfg.max_age.min(Duration::from_secs(1)).max(Duration::from_millis(1));
    let mut last_age_check = Instant::now();

    loop {
        match rx.recv_timeout(wait) {
            Ok(Msg::Quote { venue, symbol, quote }) => {
                ingest(
                    &ctx,
                    &mut state.quotes,
                    &mut state.flush_seq,
                    cfg,
                    (venue, symbol),
                    quote,
                    &QUOTE,
                );
            }
            Ok(Msg::Trade { venue, symbol, trade }) => {
                ingest(
                    &ctx,
                    &mut state.trades,
                    &mut state.flush_seq,
                    cfg,
                    (venue, symbol),
                    trade,
                    &TRADE,
                );
            }
            Ok(Msg::Book { venue, symbol, update }) => {
                ingest(
                    &ctx,
                    &mut state.books,
                    &mut state.flush_seq,
                    cfg,
                    (venue, symbol),
                    update,
                    &BOOK,
                );
            }
            Ok(Msg::Depth { venue, symbol, update }) => {
                ingest(
                    &ctx,
                    &mut state.depth,
                    &mut state.flush_seq,
                    cfg,
                    (venue, symbol),
                    update,
                    &DEPTH,
                );
            }
            Ok(Msg::Equity { venue, symbol, sample }) => {
                ingest(
                    &ctx,
                    &mut state.equity,
                    &mut state.flush_seq,
                    cfg,
                    (venue, symbol),
                    sample,
                    &EQUITY,
                );
            }
            Ok(Msg::Shutdown) => {
                flush_all(&ctx, &mut state);
                // AFTER the final flush: it is the last thing that can bump `discarded`.
                meter.report_final();
                return;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                // Every RecorderSink (and its Arc clones) is gone — nothing left to flush for.
                flush_all(&ctx, &mut state);
                meter.report_final();
                return;
            }
        }

        if age_scan_due(last_age_check.elapsed(), wait) {
            flush_aged(&ctx, &mut state.quotes, &mut state.flush_seq, cfg.max_age, &QUOTE);
            flush_aged(&ctx, &mut state.trades, &mut state.flush_seq, cfg.max_age, &TRADE);
            flush_aged(&ctx, &mut state.books, &mut state.flush_seq, cfg.max_age, &BOOK);
            flush_aged(&ctx, &mut state.depth, &mut state.flush_seq, cfg.max_age, &DEPTH);
            flush_aged(&ctx, &mut state.equity, &mut state.flush_seq, cfg.max_age, &EQUITY);
            last_age_check = Instant::now();
        }
        // Rides the same wake-up as the age scan: cheap (two relaxed loads) when nothing was lost,
        // and silent unless a delta is non-zero.
        meter.report_if_due();
    }
}

/// Whether the periodic age scan is due — the whole rule behind "a steadily-active series must not
/// starve a quiet one's age flush", named so it can be gated on its own.
///
/// **What it does NOT take is the rule.** There is deliberately no `recv` outcome here:
/// `recv_timeout` returns `Ok` the instant ANY series has a message queued, so with N series
/// multiplexed on one channel, a cadence gated on `Err(Timeout)` never fires at all while one
/// series is steadily active — a quiet series' buffered rows would then sit unflushed (and
/// unprotected by the WAL) for as long as the busy one keeps talking. Driving the cadence off
/// elapsed wall time alone is what bounds the scan to once per `wait` no matter how busy the
/// channel is. `age_scan_rule_tests::the_age_scan_rule_rejects_a_timeout_only_cadence` pins that
/// by requiring this rule and the historical broken one to disagree.
fn age_scan_due(since_last_scan: Duration, wait: Duration) -> bool {
    since_last_scan >= wait
}

/// One tick kind's writer-thread recipe: the commit-key tag, the row ts-accessor (rollover check +
/// flush-key `first_ts`/`last_ts`), and the `HistStore::append_*` method it flushes through.
/// `quote`/`trade`/`book`/`equity` (the [`QUOTE`]/[`TRADE`]/[`BOOK`]/[`EQUITY`] consts below) differ
/// ONLY in these three — bundling them into one value is what lets `ingest`/`flush_aged`/
/// `flush_all_of`/`flush_buf` be written once, generic over `T`, instead of once per kind (and
/// keeps each of those four under clippy's argument-count limit).
struct RecKind<T> {
    tag: &'static str,
    ts: fn(&T) -> i64,
    append: AppendFn<T>,
    /// The GROUPED twin of `append` — same shape, but its second argument is a group rather than a
    /// symbol. `None` for kinds that have no grouped form (equity, whose series is the portfolio).
    append_grouped: Option<AppendFn<T>>,
    /// Stamp the buffer's symbol onto a row that carries none. REQUIRED before a grouped append:
    /// a per-symbol caller may legally leave a row's symbol empty (the store tags it from the path),
    /// but a grouped series has no path to tag from and REJECTS unattributable rows.
    stamp_symbol: Option<fn(&mut T, &str)>,
}

/// The `HistStore::append_*` method shape every [`RecKind::append`] points at.
type AppendFn<T> = fn(&DataFusionHist, &str, &str, &[T], Option<&str>) -> Result<usize, DataError>;

const QUOTE: RecKind<QuoteTick> = RecKind {
    tag: "quote",
    ts: |q| q.ts,
    append: DataFusionHist::append_quotes,
    append_grouped: Some(DataFusionHist::append_quotes_grouped),
    stamp_symbol: Some(|q, s| q.symbol = s.to_string()),
};
const TRADE: RecKind<TradeTick> = RecKind {
    tag: "trade",
    ts: |t| t.ts,
    append: DataFusionHist::append_trades,
    append_grouped: Some(DataFusionHist::append_trades_grouped),
    stamp_symbol: Some(|t, s| t.symbol = s.to_string()),
};
const BOOK: RecKind<BookUpdate> = RecKind {
    tag: "book",
    ts: |u| u.ts,
    append: DataFusionHist::append_book_updates,
    append_grouped: Some(DataFusionHist::append_book_updates_grouped),
    stamp_symbol: Some(|u, s| u.symbol = s.to_string()),
};
/// Equity has no grouped form: its series IS the portfolio (`venue = "portfolio"`), so there is
/// nothing to group and no symbol to tell rows apart by.
const DEPTH: RecKind<BookUpdate> = RecKind {
    tag: "depth",
    ts: |u| u.ts,
    append: DataFusionHist::append_depth,
    // No grouped twin yet: `append_depth_grouped` does not exist, so a depth family records
    // per-symbol. That is a deliberate first step, not an oversight — the grouped write path is
    // additive and the lane has no live producer wired to it until a venue depth feed is subscribed.
    append_grouped: None,
    stamp_symbol: Some(|u, s| u.symbol = s.to_string()),
};
const EQUITY: RecKind<EquitySample> = RecKind {
    tag: "equity",
    ts: |s| s.ts,
    append: DataFusionHist::append_equity,
    append_grouped: None,
    stamp_symbol: None,
};

/// Ingest one row into its `(venue, symbol)` buffer — the shared shape behind the `Msg::*` match
/// arms in [`writer_loop`] above (every kind does the SAME rollover-check / push / max-rows-check
/// dance, differing only in `kind`): a UTC-date rollover flushes the old buffer first, then the row
/// is pushed, then a max-rows-full buffer flushes too.
///
/// ⚠ Both of those flushes go through [`flush_batch`], NOT `flush_buf`, so that **`flush_batch` is
/// the ONE place deciding grouped-vs-per-symbol**. They used to call `flush_buf` directly — the
/// per-symbol append, which never consults `ctx.grouping` — so a buffer that filled to `max_rows`
/// before `max_age` elapsed silently wrote itself PER-SYMBOL while its quieter siblings in the same
/// family wrote grouped. That is exactly backwards: a high-volume symbol is the one grouping exists
/// for, and it was the only one opting out. Found by the first live recorder run (2026-08-02): two
/// busy Polymarket tokens produced `symbol=<token>/` book series of 620 KB each ALONGSIDE the
/// family's `group=btc-updown-5m/` series of 136 KB, in one process, for the whole session — a
/// customer would then have to read two layouts to see one family, and the maintenance/retention
/// paths would treat them as unrelated series.
///
/// A one-element batch merges nothing, so this costs one `Vec` allocation per immediate flush and
/// buys the invariant that the routing decision cannot be made in two places again.
fn ingest<T>(
    ctx: &FlushCtx<'_>,
    map: &mut HashMap<(String, String), Buffered<T>>,
    flush_seq: &mut u64,
    cfg: &RecorderConfig,
    key: (String, String),
    item: T,
    kind: &RecKind<T>,
) {
    let date = epoch_ms_to_utc_date((kind.ts)(&item));

    if map.get(&key).is_some_and(|b| b.date != date)
        && let Some(buf) = map.remove(&key)
    {
        flush_batch(ctx, vec![(key.clone(), buf)], flush_seq, kind);
    }

    // Liveness BEFORE the buffer push: this counts rows that ARRIVED, which is the question
    // ("is this series receiving anything?"). Whether they have flushed yet is a different one.
    {
        let mut live = ctx.live.lock().unwrap();
        let e = live
            .entry(format!("{}/{}/{}", kind.tag, key.0, key.1))
            .or_insert(Liveness { rows: 0, last_ms: 0 });
        e.rows += 1;
        e.last_ms = vike_model::clock::now_ms();
    }

    let buf = map.entry(key.clone()).or_insert_with(|| Buffered::new(date));
    buf.rows.push(item);

    if buf.rows.len() >= cfg.max_rows
        && let Some(buf) = map.remove(&key)
    {
        flush_batch(ctx, vec![(key, buf)], flush_seq, kind);
    }
}

/// Flush every buffer in `map` whose oldest row has aged past `max_age` — the shared shape behind
/// the four age-check call sites in [`writer_loop`] above.
fn flush_aged<T>(
    ctx: &FlushCtx<'_>,
    map: &mut HashMap<(String, String), Buffered<T>>,
    flush_seq: &mut u64,
    max_age: Duration,
    kind: &RecKind<T>,
) {
    let stale: Vec<(String, String)> = map
        .iter()
        .filter(|(_, b)| b.opened_at.elapsed() >= max_age)
        .map(|(k, _)| k.clone())
        .collect();
    let entries: Vec<_> =
        stale.into_iter().filter_map(|k| map.remove(&k).map(|b| (k, b))).collect();
    flush_batch(ctx, entries, flush_seq, kind);
}

fn flush_all(ctx: &FlushCtx<'_>, state: &mut WriterState) {
    flush_all_of(ctx, &mut state.quotes, &mut state.flush_seq, &QUOTE);
    flush_all_of(ctx, &mut state.trades, &mut state.flush_seq, &TRADE);
    flush_all_of(ctx, &mut state.books, &mut state.flush_seq, &BOOK);
    flush_all_of(ctx, &mut state.depth, &mut state.flush_seq, &DEPTH);
    flush_all_of(ctx, &mut state.equity, &mut state.flush_seq, &EQUITY);
}

/// Drain and flush every buffer in `map` — the shared shape behind [`flush_all`]'s four calls
/// (shutdown / sender-disconnected teardown: flush every remaining buffer of one kind, in
/// whatever order the map yields them).
fn flush_all_of<T>(
    ctx: &FlushCtx<'_>,
    map: &mut HashMap<(String, String), Buffered<T>>,
    flush_seq: &mut u64,
    kind: &RecKind<T>,
) {
    flush_batch(ctx, map.drain().collect(), flush_seq, kind);
}

/// Run `attempt`, and on failure run it EXACTLY once more after `retry_delay`. Returns the last
/// result and how many attempts were made (1 or 2).
///
/// Pure with respect to the store (it only calls the closure), which is what makes the retry policy
/// testable without an I/O-failing `DataFusionHist`.
///
/// **The retry is skipped when the first attempt itself took `slow_attempt` or longer.** That is the
/// discriminator between the two failure shapes, and it needs no fragile matching on `DataError`'s
/// opaque strings: a transient I/O error fails fast and is worth a second go, while a lock timeout
/// only returns after `SeriesLock::acquire` has already spun ~4s, so retrying it would mostly just
/// double the single writer thread's stall — and every millisecond stalled here is more rows dropped
/// at the channel behind it.
fn append_with_retry<F>(
    mut attempt: F,
    retry_delay: Duration,
    slow_attempt: Duration,
) -> (Result<usize, DataError>, u32)
where
    F: FnMut() -> Result<usize, DataError>,
{
    let started = Instant::now();
    let first = attempt();
    if first.is_ok() || started.elapsed() >= slow_attempt {
        return (first, 1);
    }
    thread::sleep(retry_delay);
    (attempt(), 2)
}

/// Flush one buffered `(venue, symbol)` series through `kind.append` — the shared shape behind
/// every flush call site above (`quote`/`trade`/`book`/`equity` differ only in `kind`, see
/// [`RecKind`]).
fn flush_buf<T>(
    ctx: &FlushCtx<'_>,
    venue: &str,
    symbol: &str,
    buf: Buffered<T>,
    seq: u64,
    kind: &RecKind<T>,
) {
    flush_buf_with(ctx, venue, symbol, buf.rows, seq, kind, kind.append);
}

/// [`flush_buf`] over an explicit row vec and append fn, so the per-symbol and GROUPED paths share
/// one implementation of the retry, the commit-key scheme and the loss accounting. `target` is the
/// symbol on the per-symbol path and the group on the grouped one — it is the second argument of
/// the store append either way, and what makes the commit key unique per series per flush.
fn flush_buf_with<T>(
    ctx: &FlushCtx<'_>,
    venue: &str,
    symbol: &str,
    rows_in: Vec<T>,
    seq: u64,
    kind: &RecKind<T>,
    append: AppendFn<T>,
) {
    let buf = Buffered { rows: rows_in, date: String::new(), opened_at: Instant::now() };
    if buf.rows.is_empty() {
        return;
    }
    let first_ts = (kind.ts)(buf.rows.first().expect("checked non-empty"));
    let last_ts = (kind.ts)(buf.rows.last().expect("checked non-empty"));
    // `seq` (this writer thread's monotonic flush counter) guarantees uniqueness even when two
    // flushes of the same series share (first_ts, last_ts) — see the module doc for why that
    // matters (a repeated commit key is a silent no-op in the store, not an error). The RETRY below
    // reuses this same key on purpose: that no-op is exactly what makes retrying safe, since an
    // attempt that failed after committing cannot be double-written.
    let key = format!("live-{venue}-{symbol}-{}-{first_ts}-{last_ts}-{seq}", kind.tag);
    let rows = buf.rows.len();
    let (result, attempts) = append_with_retry(
        || append(ctx.store, venue, symbol, &buf.rows, Some(&key)),
        FLUSH_RETRY_DELAY,
        SLOW_ATTEMPT,
    );
    match result {
        Ok(_) if attempts > 1 => {
            tracing::info!(
                venue,
                symbol,
                rows,
                "RecorderSink: {} flush succeeded on retry — no rows lost",
                kind.tag
            );
        }
        Ok(_) => {}
        Err(err) => {
            // Nowhere left to put them: the rows are gone. Count them so the loss shows up in the
            // periodic `DropReport` instead of only as this one log line.
            ctx.discarded.fetch_add(rows as u64, Ordering::Relaxed);
            tracing::warn!(
                venue,
                symbol,
                %err,
                rows,
                attempts = attempts as u64,
                "RecorderSink: {} flush failed after {attempts} attempt(s) — DISCARDING {rows} rows \
                 (this series' tape now has a hole). A PERSISTENT failure means another writer is \
                 holding this series' lock for longer than the ~4s spin — check for a second \
                 process writing the same store root, or a compaction stuck on a huge `date=`.",
                kind.tag
            );
        }
    }
}

/// Flush a batch of drained buffers, MERGING any that resolve to the same group into one commit.
///
/// This is the live half of `BulkIngestSession`'s `merge_and_commit`, and it exists for the same
/// reason: routing per-symbol buffers at a shared group directory without merging them would be
/// strictly WORSE than not grouping — each buffer would still be its own commit, now all contending
/// on ONE `SeriesLock` instead of N independent ones. The merge is the feature.
///
/// Ungrouped (`ctx.grouping` is `None`, or the resolver returns `None`, or the kind has no grouped
/// form) each buffer flushes exactly as before — byte-identical to the pre-grouping path.
fn flush_batch<T>(
    ctx: &FlushCtx<'_>,
    entries: Vec<((String, String), Buffered<T>)>,
    flush_seq: &mut u64,
    kind: &RecKind<T>,
) {
    let grouped_append = kind.append_grouped;
    // (venue, group) -> merged rows. BTreeMap so commit order is deterministic run to run.
    let mut groups: BTreeMap<(String, String), Vec<T>> = BTreeMap::new();

    for ((venue, symbol), mut buf) in entries {
        let group = grouped_append
            .and(ctx.grouping.as_ref())
            .and_then(|f| f(&venue, &symbol))
            .filter(|_| kind.stamp_symbol.is_some());
        match group {
            Some(g) => {
                // A grouped series tells rows apart by their symbol column, so every row must carry
                // one. A per-symbol caller may legally have left it empty.
                if let Some(stamp) = kind.stamp_symbol {
                    for row in &mut buf.rows {
                        stamp(row, &symbol);
                    }
                }
                groups.entry((venue, g)).or_default().extend(buf.rows);
            }
            None => {
                let seq = next_seq(flush_seq);
                flush_buf(ctx, &venue, &symbol, buf, seq, kind);
            }
        }
    }

    for ((venue, group), rows) in groups {
        if rows.is_empty() {
            continue;
        }
        let seq = next_seq(flush_seq);
        // `flush_buf` over the GROUP: same retry, same key shape, same loss accounting — the group
        // simply takes the symbol's place, which is also what makes the commit key unique per flush.
        flush_buf_with(
            ctx,
            &venue,
            &group,
            rows,
            seq,
            kind,
            grouped_append.expect("group only set when the kind has a grouped append"),
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::StreamStatus;
    use crate::{DataFusionHist, HistStore, TsRange};
    use vike_model::{BookUpdate, BookUpdateKind, EquitySample, L2Book, QuoteTick, TradeTick};

    const DAY: i64 = 86_400_000;

    fn quote(ts: i64, bid: f64) -> QuoteTick {
        QuoteTick {
            ts,
            local_ts: 0,
            bid,
            ask: bid + 0.5,
            bid_size: 1.0,
            ask_size: 2.0,
            symbol: String::new(),
        }
    }

    fn trade(ts: i64, price: f64) -> TradeTick {
        TradeTick {
            ts,
            local_ts: 0,
            price,
            size: 1.0,
            is_buyer_maker: false,
            symbol: String::new(),
        }
    }

    fn sample(ts: i64, equity: f64, missing_prices: u32) -> EquitySample {
        EquitySample {
            ts,
            venue: String::new(),
            equity,
            realized: equity - 1.0,
            unrealized: 1.0,
            missing_prices,
        }
    }

    fn open_store() -> (tempfile::TempDir, Arc<DataFusionHist>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        (dir, store)
    }

    fn assert_quotes_bit_eq(want: &[QuoteTick], got: &[QuoteTick]) {
        assert_eq!(want.len(), got.len(), "quote count");
        for (i, (x, y)) in want.iter().zip(got).enumerate() {
            assert_eq!(x.ts, y.ts, "ts[{i}]");
            assert_eq!(x.bid.to_bits(), y.bid.to_bits(), "bid[{i}]");
            assert_eq!(x.ask.to_bits(), y.ask.to_bits(), "ask[{i}]");
            assert_eq!(x.bid_size.to_bits(), y.bid_size.to_bits(), "bid_size[{i}]");
            assert_eq!(x.ask_size.to_bits(), y.ask_size.to_bits(), "ask_size[{i}]");
        }
    }

    fn assert_trades_bit_eq(want: &[TradeTick], got: &[TradeTick]) {
        assert_eq!(want.len(), got.len(), "trade count");
        for (i, (x, y)) in want.iter().zip(got).enumerate() {
            assert_eq!(x.ts, y.ts, "ts[{i}]");
            assert_eq!(x.price.to_bits(), y.price.to_bits(), "price[{i}]");
            assert_eq!(x.size.to_bits(), y.size.to_bits(), "size[{i}]");
            assert_eq!(x.is_buyer_maker, y.is_buyer_maker, "is_buyer_maker[{i}]");
        }
    }

    /// Count `.parquet` files under a dir (recurses `date=` subdirs) — same helper shape as
    /// `tests/hist_datafusion.rs`'s `count_parquets`, used to observe the rollover producing two
    /// separate sealed parts.
    fn count_parquets(dir: &std::path::Path) -> usize {
        let mut n = 0;
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    n += count_parquets(&p);
                } else if p.extension().is_some_and(|x| x == "parquet") {
                    n += 1;
                }
            }
        }
        n
    }

    fn quote_series_dir(root: &std::path::Path, venue: &str, symbol: &str) -> std::path::PathBuf {
        root.join("kind=quote").join(format!("venue={venue}")).join(format!("symbol={symbol}"))
    }

    #[test]
    fn flush_on_max_rows() {
        let (_dir, store) = open_store();
        let cfg = RecorderConfig { max_rows: 3, ..RecorderConfig::default() };
        let (sink, handle) = RecorderSink::spawn(store.clone(), cfg).unwrap();

        let sent = vec![quote(1_000, 100.0), quote(2_000, 100.5), quote(3_000, 101.0)];
        for q in &sent {
            sink.quote("polymarket", "TOK", q.clone());
        }

        // Bounded poll BEFORE any shutdown: this isolates the max-rows trigger itself. Polling
        // strictly before shutdown() is what proves the flush happened because the buffer hit
        // max_rows — shutdown's own flush-all would otherwise mask a broken max_rows path.
        let start = Instant::now();
        let got = loop {
            let got = store.scan_quotes("polymarket", "TOK", TsRange::all()).unwrap();
            if got.len() >= sent.len() {
                break got;
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "max-rows flush did not fire in time"
            );
            thread::sleep(Duration::from_millis(50));
        };
        assert_quotes_bit_eq(&sent, &got);

        handle.shutdown();
    }

    #[test]
    fn flush_on_age() {
        let (_dir, store) = open_store();
        let cfg =
            RecorderConfig { max_age: Duration::from_millis(200), ..RecorderConfig::default() };
        let (sink, handle) = RecorderSink::spawn(store.clone(), cfg).unwrap();

        let sent = quote(5_000, 42.0);
        sink.quote("polymarket", "TOK", sent.clone());

        // Bounded poll: the writer's recv_timeout wakes at min(max_age, 1s) = 200ms, so the row
        // should be visible well within a couple of seconds without an explicit shutdown.
        let start = Instant::now();
        let got = loop {
            let got = store.scan_quotes("polymarket", "TOK", TsRange::all()).unwrap();
            if !got.is_empty() {
                break got;
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "age-based flush did not fire in time"
            );
            thread::sleep(Duration::from_millis(20));
        };
        assert_quotes_bit_eq(&[sent], &got);

        handle.shutdown();
    }

    /// Final-review fix: `recv_timeout` returns `Ok` immediately whenever ANY series has a message
    /// queued, so with N series multiplexed on one channel a steadily-active series must not starve
    /// the age-flush pass for a quiet one. A busy series is flooded continuously (keeping `recv`
    /// returning `Ok`, never `Err(Timeout)`) while a quiet series sits on a single old row with a
    /// short `max_age`; the quiet row must still surface via `scan_quotes` within a bounded poll,
    /// WHILE the busy series is still running (no shutdown involved) — this is what proves the age
    /// scan runs on a wall-clock cadence, not only in the `Err(Timeout)` arm. Against the old
    /// Timeout-only code the quiet row NEVER surfaces (recv essentially never times out here), so
    /// this is a condition wait: the loop exits on the row being there, never on the clock, and the
    /// deadline can only ever turn a hang into a named failure.
    ///
    /// ⚠ **The busy series floods a different KIND on purpose, and it is what took the flake out.**
    /// It used to flood quotes, which put TWO incidental store commits between the age scan and the
    /// assertion: at the same scan the busy buffer is also `>= max_age` old, so `flush_aged` flushed
    /// it too — and `flush_aged` walks a `HashMap`, whose iteration order is `RandomState`-seeded
    /// per PROCESS, so on a coin flip the busy series' whole commit ran BEFORE the quiet one's even
    /// began. Instrumented on the CI box (1,032 runs of the CI shape under load), the slowest run
    /// decomposed as: age scan on time at +200 ms, then `BUSY rows=2040 append_ms=983`, then
    /// `QUIET rows=1 append_ms=2008` — a single ONE-ROW commit costing 2 s because
    /// `DataFusionHist::commit_rows` fsyncs five times (WAL, parquet part, `date=` dir, manifest
    /// tmp, series dir) and an fsync on that box costs 7 ms idle. None of that is the property under
    /// test. Flooding trades instead pins the order: `writer_loop`'s age pass runs `flush_aged` over
    /// quotes BEFORE trades, so the quiet quote is always committed first, while the starvation
    /// condition is untouched — it is about the shared CHANNEL never going quiet, which is
    /// kind-independent.
    #[test]
    fn age_flush_fires_under_busy_other_series() {
        let (_dir, store) = open_store();
        let cfg =
            RecorderConfig { max_age: Duration::from_millis(200), ..RecorderConfig::default() };
        let (sink, handle) = RecorderSink::spawn(store.clone(), cfg).unwrap();

        // Quiet series: one row, sent once, then never touched again.
        let quiet = quote(9_000, 7.0);
        sink.quote("polymarket", "QUIET", quiet.clone());

        // Busy series: flood a steady stream (faster than `wait`) so the writer's `recv_timeout`
        // keeps returning `Ok` — this is exactly the condition that starved the age flush before
        // the fix. A different kind (see the doc above) keeps its commit off the quiet series'
        // critical path without weakening that condition.
        let busy_sink = sink.clone();
        let busy_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let busy_stop_thread = busy_stop.clone();
        let busy = thread::spawn(move || {
            let mut i = 0i64;
            while !busy_stop_thread.load(Ordering::Relaxed) {
                busy_sink.trade("polymarket", "BUSY", trade(i, i as f64));
                i += 1;
                thread::sleep(Duration::from_millis(1));
            }
        });

        // Condition poll for the quiet series' row — must become visible while the busy stream is
        // STILL running (checked below), never via a shutdown-triggered flush-all.
        //
        // The deadline is [`REPORT_DEADLINE`], and it is doing the same job it does there: bounding
        // how long an ACTUALLY BROKEN writer takes to fail, not stating how long this should take.
        // It cannot manufacture a pass — the loop breaks only on the row being present — so the
        // failure it guards is "never", and against the Timeout-only writer no deadline is reached
        // by anything but the assert. The observation channel is a full DataFusion read (fresh
        // `SessionContext` + manifest re-read + parquet scan, measured at up to 1.2 s for ONE poll
        // under contention), so the poll interval is deliberately coarse: the test's own polling
        // competes with the writer thread it is waiting for.
        let start = Instant::now();
        let got = loop {
            let got = store.scan_quotes("polymarket", "QUIET", TsRange::all()).unwrap();
            if !got.is_empty() {
                break got;
            }
            assert!(
                start.elapsed() < REPORT_DEADLINE,
                "age flush starved by a busy other series (quiet series' row never surfaced \
                 after {:?})",
                start.elapsed()
            );
            thread::sleep(Duration::from_millis(50));
        };
        assert_quotes_bit_eq(&[quiet], &got);

        busy_stop.store(true, Ordering::Relaxed);
        busy.join().unwrap();
        handle.shutdown();
    }

    /// The age-scan cadence rule, gated by arithmetic — no store, no writer thread, no sleeping.
    ///
    /// [`age_flush_fires_under_busy_other_series`] above proves the rule end to end, but it can only
    /// do so through a wall clock and five fsyncs. The rule itself is decidable without either, and
    /// this is where it is decided.
    mod age_scan_rule_tests {
        use super::*;

        /// The historical BROKEN cadence: the age scan ran ONLY in `recv_timeout`'s `Err(Timeout)`
        /// arm. Defined here, in the test, because it is the thing the real rule must disagree with.
        fn timeout_only_age_scan_due(
            since_last_scan: Duration,
            wait: Duration,
            recv_timed_out: bool,
        ) -> bool {
            recv_timed_out && since_last_scan >= wait
        }

        /// Walk one writer-loop's worth of iterations under a channel that NEVER goes quiet (every
        /// `recv` returns `Ok`, as it does whenever any of N multiplexed series is steadily active)
        /// and count how many age scans each rule would run. `rule` takes the same three facts, so
        /// both are driven by one checker over one scripted sequence.
        fn scans_over_a_busy_channel(rule: impl Fn(Duration, Duration, bool) -> bool) -> usize {
            let wait = Duration::from_millis(200);
            let mut since = Duration::ZERO;
            let mut scans = 0;
            // 1000 iterations at 10ms of simulated progress each = 10s of a channel that always has
            // a message ready — 50 `wait` periods' worth.
            for _ in 0..1000 {
                since += Duration::from_millis(10);
                if rule(since, wait, /* recv_timed_out */ false) {
                    scans += 1;
                    since = Duration::ZERO;
                }
            }
            scans
        }

        /// The negative control: one checker, pointed at the real rule and at the historical broken
        /// one, required to reach OPPOSITE verdicts. Without this, a rule that silently regained a
        /// `recv`-outcome dependency would still pass every timing-based test on a fast box.
        #[test]
        fn the_age_scan_rule_rejects_a_timeout_only_cadence() {
            let real = scans_over_a_busy_channel(|since, wait, _| age_scan_due(since, wait));
            let broken = scans_over_a_busy_channel(timeout_only_age_scan_due);

            assert_eq!(
                broken, 0,
                "the Timeout-only cadence must run ZERO age scans while the channel never goes \
                 quiet — that is the bug this rule replaced"
            );
            assert_eq!(
                real, 50,
                "the real rule must run one age scan per `wait` regardless of `recv`'s outcome \
                 (10s of simulated progress / 200ms wait)"
            );
        }

        /// The cadence is bounded ABOVE by `wait` too: a scan is due the moment `wait` has elapsed,
        /// never later, so a quiet series' exposure is `max_age + wait` and not a function of how
        /// much traffic the busy siblings pushed through in between.
        #[test]
        fn a_scan_is_due_exactly_once_wait_has_elapsed() {
            let wait = Duration::from_millis(200);
            assert!(!age_scan_due(Duration::from_millis(199), wait));
            assert!(age_scan_due(Duration::from_millis(200), wait));
            assert!(age_scan_due(Duration::from_secs(30), wait));
        }
    }

    #[test]
    fn utc_rollover_splits_flushes() {
        let (dir, store) = open_store();
        // max_rows big enough that only the rollover (not row-count) drives the first flush.
        let cfg = RecorderConfig { max_rows: 1_000, ..RecorderConfig::default() };
        let (sink, handle) = RecorderSink::spawn(store.clone(), cfg).unwrap();

        let day0 = quote(1_000, 10.0); // 1970-01-01
        let day1 = quote(DAY + 1_000, 20.0); // 1970-01-02 — different UTC date -> rollover flush
        sink.quote("polymarket", "TOK", day0.clone());
        sink.quote("polymarket", "TOK", day1.clone());
        // Shutdown flushes the still-open day1 buffer; by the time it joins, the rollover flush of
        // day0 (triggered by day1 arriving) already ran — same ordered channel.
        handle.shutdown();

        let series = quote_series_dir(dir.path(), "polymarket", "TOK");
        assert_eq!(count_parquets(&series), 2, "one sealed part per UTC day (two commit keys)");
        assert!(series.join("date=1970-01-01").exists());
        assert!(series.join("date=1970-01-02").exists());

        let got = store.scan_quotes("polymarket", "TOK", TsRange::all()).unwrap();
        assert_quotes_bit_eq(&[day0, day1], &got);
    }

    #[test]
    fn shutdown_flushes_remainder() {
        let (_dir, store) = open_store();
        let cfg = RecorderConfig { max_rows: 1_000, ..RecorderConfig::default() };
        let (sink, handle) = RecorderSink::spawn(store.clone(), cfg).unwrap();

        let sent = vec![trade(1_000, 5.0), trade(1_500, 5.1)];
        for t in &sent {
            sink.trade("polymarket", "TOK", t.clone());
        }
        // Neither max_rows nor max_age has fired yet — only shutdown's flush-all makes this visible.
        handle.shutdown();

        let got = store.scan_trades("polymarket", "TOK", TsRange::all()).unwrap();
        assert_trades_bit_eq(&sent, &got);
    }

    /// ⚠ **One producer, one series — so this test cannot see an attribution bug at all**, and for
    /// a long time nothing else could either: it asserts `dropped() > 0`, which a counter covering
    /// every series satisfies just as well as a keyed one. The two-producer twin
    /// (`two_producers_overflow_and_the_quiet_series_is_named`) is where the key is gated; this one
    /// stays as the "never blocks, never panics, still joins" case it was written for.
    #[test]
    fn overflow_drops_and_counts() {
        let (_dir, store) = open_store();
        let cfg = RecorderConfig { channel_cap: 1, ..RecorderConfig::default() };
        let (sink, handle) = RecorderSink::spawn(store.clone(), cfg).unwrap();

        // Flood far faster than the channel (capacity 1) can drain — some try_sends must fail.
        for i in 0..50_000i64 {
            sink.quote("polymarket", "TOK", quote(i, i as f64));
        }

        assert!(
            handle.dropped() > 0,
            "flooding a capacity-1 channel must drop rows, not block/panic"
        );
        handle.shutdown(); // must still join cleanly — no deadlock from the overflow path
    }

    #[test]
    fn book_is_noop() {
        let (dir, store) = open_store();
        let cfg = RecorderConfig::default();
        let (sink, handle) = RecorderSink::spawn(store.clone(), cfg).unwrap();

        let mut book = L2Book::new(0.01);
        book.apply_snapshot(1, &[(10.0, 1.0)], &[(10.5, 1.0)]);
        sink.book("polymarket", "TOK", Arc::new(book));
        handle.shutdown();

        // No book schema exists in the store at all — nothing was ever written for this series.
        assert!(!dir.path().join("kind=book").exists());
        let series = store.list_series().unwrap();
        assert!(series.is_empty(), "book() must not create any series");
    }

    fn book_upd(ts: i64, seq: u64, kind: BookUpdateKind) -> BookUpdate {
        BookUpdate {
            ts,
            local_ts: ts + 1,
            seq,
            kind,
            tick_size: 0.01,
            bids: vec![(0.45, 10.0)],
            asks: vec![(0.46, 5.0)],
            symbol: String::new(),
        }
    }

    #[test]
    fn book_updates_flush_and_roundtrip() {
        let (_dir, store) = open_store();
        let cfg = RecorderConfig { max_rows: 2, ..RecorderConfig::default() };
        let (sink, handle) = RecorderSink::spawn(store.clone(), cfg).unwrap();
        sink.book_update("polymarket", "TOK", book_upd(1_000, 1, BookUpdateKind::Snapshot));
        sink.book_update("polymarket", "TOK", book_upd(1_005, 2, BookUpdateKind::Delta));
        handle.shutdown();
        let got = store.scan_book_updates("polymarket", "TOK", TsRange::all()).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].kind, BookUpdateKind::Snapshot);
        assert_eq!(got[1].seq, 2);
        assert_eq!(got[0].bids[0].0.to_bits(), 0.45f64.to_bits());
    }

    #[test]
    fn book_stream_status_recorded_as_markers() {
        let (_dir, store) = open_store();
        let (sink, handle) = RecorderSink::spawn(store.clone(), RecorderConfig::default()).unwrap();
        sink.book_update("polymarket", "TOK", book_upd(1_000, 1, BookUpdateKind::Snapshot));
        sink.stream_status("polymarket", "TOK", "book", StreamStatus::GapStart { at_ts_ms: 2_000 });
        sink.stream_status(
            "polymarket",
            "TOK",
            "book",
            StreamStatus::Live { gap_started_ts_ms: Some(2_000) },
        );
        // A non-L2 lane still records nothing here (quotes/trades gap provenance = follow-up), and
        // it must not leak into the BOOK series either:
        sink.stream_status(
            "polymarket",
            "TOK",
            "quotes",
            StreamStatus::GapStart { at_ts_ms: 9_000 },
        );
        handle.shutdown();
        let got = store.scan_book_updates("polymarket", "TOK", TsRange::all()).unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[1].kind, BookUpdateKind::GapStart);
        assert_eq!(got[1].ts, 2_000);
        assert!(got[1].bids.is_empty() && got[1].asks.is_empty());
        assert_eq!(got[2].kind, BookUpdateKind::LiveResume);
    }

    /// **The DEPTH lane's markers land in the DEPTH series** — the disclosure half of the
    /// forty-day binance perp reconnect loop.
    ///
    /// `stream_status` early-returned for every stream but `"book"`, so `depth_main`'s `GapStart`
    /// (one per reconnect cycle, ~20,000 a day on the broken lane) was written into no series at
    /// all: a backtest reading `kind=depth` saw a book teleporting every 4.3 s with nothing to say
    /// the live feed had been down between the rows.
    ///
    /// Three assertions, each failing on a different way of reopening it: the markers are RECORDED,
    /// they are recorded under `kind=depth` and NOT under `kind=book` (a marker in the wrong series
    /// would corrupt the book lane's own chain and is the tempting one-line "fix"), and a non-L2
    /// stream label still records nothing.
    ///
    /// MUTATION PROOF: restore `if stream != "book" { return; }` and the depth scan comes back
    /// empty on the two markers; route the `"depth"` arm to `Msg::Book` and the `kind=book`
    /// assertion goes red.
    #[test]
    fn depth_stream_status_is_recorded_into_the_depth_series() {
        let (_dir, store) = open_store();
        let (sink, handle) = RecorderSink::spawn(store.clone(), RecorderConfig::default()).unwrap();

        sink.l2_snapshot(
            "binance",
            "BTCUSDT.P",
            0.1,
            vec![(60000.0, 1.0)],
            vec![(60001.0, 1.0)],
            1_000,
        );
        sink.stream_status(
            "binance",
            "BTCUSDT.P",
            "depth",
            StreamStatus::GapStart { at_ts_ms: 2_000 },
        );
        sink.stream_status(
            "binance",
            "BTCUSDT.P",
            "depth",
            StreamStatus::Live { gap_started_ts_ms: Some(2_000) },
        );
        // …and a lane with no BookUpdate series of its own is still dropped.
        sink.stream_status(
            "binance",
            "BTCUSDT.P",
            "trades",
            StreamStatus::GapStart { at_ts_ms: 9_000 },
        );
        handle.shutdown();

        let got = store.scan_depth("binance", "BTCUSDT.P", TsRange::all()).unwrap();
        assert_eq!(got.len(), 3, "the snapshot plus BOTH depth markers: {got:?}");
        assert_eq!(got[1].kind, BookUpdateKind::GapStart);
        assert_eq!(got[1].ts, 2_000);
        assert!(got[1].bids.is_empty() && got[1].asks.is_empty(), "a marker carries no levels");
        assert_eq!(got[2].kind, BookUpdateKind::LiveResume);

        // No BOOK series exists at all — a depth marker written there would inject a `seq: 0` row
        // into a lane whose whole promise is a contiguous chain.
        let kinds: Vec<String> = store.list_series().unwrap().into_iter().map(|s| s.kind).collect();
        assert_eq!(kinds, vec!["depth".to_string()], "depth markers must not create a book series");
    }

    /// End-to-end producer -> writer-thread actor -> shutdown-flush -> store round-trip for the
    /// equity lane (portfolio-observer PR-3, Task 3) — the same shape as the quote/trade lane
    /// tests above, proving `record_equity` is wired through `Msg::Equity` / `ingest` (keyed by the
    /// `EQUITY` [`RecKind`]) into `HistStore::append_equity`, keyed by `(venue, symbol)` exactly
    /// like `HistStore::scan_equity` expects (`venue` = the fixed `"portfolio"` namespace, `symbol`
    /// = the per-exchange venue name or the cross-venue `"TOTAL"` rollup).
    #[test]
    fn recorder_persists_equity_samples() {
        let (_dir, store) = open_store();
        let (sink, handle) = RecorderSink::spawn(store.clone(), RecorderConfig::default()).unwrap();

        let venue_sample = sample(1, 100.0, 0);
        let total_sample = sample(1, 250.0, 2);
        sink.record_equity("portfolio", "binance", venue_sample.clone());
        sink.record_equity("portfolio", "TOTAL", total_sample.clone());
        handle.shutdown(); // flushes + joins

        let got_venue = store.scan_equity("portfolio", "binance", TsRange::all()).unwrap();
        let got_total = store.scan_equity("portfolio", "TOTAL", TsRange::all()).unwrap();
        assert_eq!(got_venue.len(), 1);
        assert_eq!(got_total.len(), 1);

        // `venue` on the scanned row is re-injected from the `symbol` argument (see
        // `datafusion_hist/codec.rs`'s `batch_to_equities` — same shape as `kind=properties`), not
        // carried through from the row's own `.venue` field at append time.
        assert_eq!(got_venue[0].venue, "binance");
        assert_eq!(got_total[0].venue, "TOTAL");

        assert_eq!(got_venue[0].ts, venue_sample.ts);
        assert_eq!(got_venue[0].equity.to_bits(), venue_sample.equity.to_bits());
        assert_eq!(got_venue[0].realized.to_bits(), venue_sample.realized.to_bits());
        assert_eq!(got_venue[0].unrealized.to_bits(), venue_sample.unrealized.to_bits());
        assert_eq!(got_venue[0].missing_prices, venue_sample.missing_prices);

        assert_eq!(got_total[0].equity.to_bits(), total_sample.equity.to_bits());
        assert_eq!(got_total[0].missing_prices, total_sample.missing_prices);
    }

    #[test]
    fn equity_shutdown_flushes_remainder() {
        // Mirrors `shutdown_flushes_remainder` for quotes/trades: neither max_rows nor max_age
        // has fired — only shutdown's flush-all makes the buffered equity rows visible.
        let (_dir, store) = open_store();
        let cfg = RecorderConfig { max_rows: 1_000, ..RecorderConfig::default() };
        let (sink, handle) = RecorderSink::spawn(store.clone(), cfg).unwrap();

        let sent = vec![sample(1_000, 10.0, 0), sample(1_500, 11.0, 0)];
        for s in &sent {
            sink.record_equity("portfolio", "binance", s.clone());
        }
        handle.shutdown();

        let got = store.scan_equity("portfolio", "binance", TsRange::all()).unwrap();
        assert_eq!(got.len(), sent.len());
        for (want, got) in sent.iter().zip(&got) {
            assert_eq!(want.ts, got.ts);
            assert_eq!(want.equity.to_bits(), got.equity.to_bits());
        }
    }

    #[test]
    fn equity_overflow_drops_and_counts() {
        // Mirrors `overflow_drops_and_counts`: flooding a capacity-1 channel must drop rows (via
        // the shared `send` helper's dropped-counter path), never block or panic the caller.
        let (_dir, store) = open_store();
        let cfg = RecorderConfig { channel_cap: 1, ..RecorderConfig::default() };
        let (sink, handle) = RecorderSink::spawn(store.clone(), cfg).unwrap();

        for i in 0..50_000i64 {
            sink.record_equity("portfolio", "binance", sample(i, i as f64, 0));
        }

        assert!(
            handle.dropped() > 0,
            "flooding a capacity-1 channel must drop equity rows, not block/panic"
        );
        handle.shutdown(); // must still join cleanly — no deadlock from the overflow path
    }

    // ---- loss reporting: `dropped`/`discarded` are ANNOUNCED, not merely counted ---------------
    //
    // The bug these cover: `RecorderHandle::dropped()` had exactly two call sites in the whole
    // workspace, both `#[cfg(test)]` in this file. Nothing in `vike-app`/`vike-tradehub` read it, so
    // a live recorder could gap its tape forever in silence — the failure mode `vike-alerting` was
    // split out for, with its own metric unread.

    /// Captures every [`DropReport`] the writer thread emits, so a test can assert on what an
    /// operator (or a pager) would actually have seen.
    #[derive(Clone, Default)]
    struct ReportLog(Arc<std::sync::Mutex<Vec<DropReport>>>);

    impl ReportLog {
        fn observer(&self) -> DropObserver {
            let inner = self.0.clone();
            // `clone`, not `*`: a report now carries its per-series breakdown, so it is not `Copy`.
            Arc::new(move |r: &DropReport| inner.lock().unwrap().push(r.clone()))
        }
        fn reports(&self) -> Vec<DropReport> {
            self.0.lock().unwrap().clone()
        }
        /// Named `count` rather than `len` so no reader mistakes this for a collection.
        fn count(&self) -> usize {
            self.0.lock().unwrap().len()
        }
    }

    /// A short-cadence config: the writer wakes every 20ms (`max_age`) and considers reporting every
    /// 50ms, so a test observes many report windows in well under a second.
    fn chatty(channel_cap: usize) -> RecorderConfig {
        RecorderConfig {
            channel_cap,
            max_age: Duration::from_millis(20),
            drop_report_every: Duration::from_millis(50),
            ..RecorderConfig::default()
        }
    }

    /// How long a test may wait for a [`DropReport`] to reach the observer before calling it
    /// broken. Twin of the bound `a_permanently_failed_flush_discards_and_counts_the_rows` already
    /// used, hoisted so the two cannot drift apart.
    ///
    /// It bounds only how long an ACTUALLY BROKEN test takes to fail — the waits below poll every
    /// 2ms and break the instant the condition holds — so it never slows the happy path.
    ///
    /// **10s is ~8x the worst latency ever measured, and it is not papering over a lost edge.**
    /// A report's latency is bounded by the writer thread's store commits, not by
    /// `drop_report_every` (see [`RecorderConfig::drop_report_every`]): MEASURED on the the CI box CI
    /// box, the first report of a ~19,000-row burst lands ~80ms after the burst when idle, p90
    /// 176ms / max 350ms under a 4-way parallel test load, and max 1,305ms under an 8-way one.
    /// Across 260 instrumented runs the report was late 6 times and **lost zero times** — which is
    /// the discriminator that says a deadline is the right tool here at all. Where a report CAN be
    /// permanently lost, no deadline is big enough and widening one is the bug (see
    /// `crates/vike-bridge-core/src/user_data.rs`'s `run_resync_supervisor`, whose 3s -> 30s
    /// widening then failed at 30.083s because the edge was gone, not slow).
    const REPORT_DEADLINE: Duration = Duration::from_secs(10);

    /// The series key a `sink.quote(venue, symbol, _)` row is charged to when it is dropped — the
    /// one constructor, so no test can pin a `kind` spelling the sink does not produce.
    fn quote_key(venue: &str, symbol: &str) -> SeriesId {
        SeriesId::per_symbol("quote", venue, symbol, None)
    }

    /// Whether a report NAMES this series among the rows it says were dropped.
    fn names_series(report: &DropReport, series: &SeriesId) -> bool {
        report.dropped_series.iter().any(|s| &s.series == series)
    }

    /// The single series every [`drive_meter`] burst is charged to — the arithmetic tests care about
    /// the counting, not about who lost the rows, but the counting is now keyed so SOME key must be
    /// named.
    fn drive_series() -> SeriesId {
        quote_key("polymarket", "TOK")
    }

    /// A [`DropTally`] with `n` rows already lost on [`drive_series`] — the keyed replacement for
    /// what used to be `Arc::new(AtomicU64::new(n))`.
    fn tally_of(n: u64) -> Arc<DropTally> {
        let tally = Arc::new(DropTally::new());
        for _ in 0..n {
            tally.record(Some(drive_series()));
        }
        tally
    }

    /// Drive a real [`LossMeter`] with `every = ZERO` (so every `report_if_due` is due) through
    /// `bursts`, calling `report_if_due` `silent_windows` extra times after each burst — the
    /// windows in which nothing new was lost but the CUMULATIVE total is still non-zero. Ends with
    /// the closing `report_final`. No store, no writer thread, no wall clock: the whole delta
    /// discipline is decided by arithmetic here, which is why this is where it is gated.
    fn drive_meter(bursts: &[u64], silent_windows: usize) -> Vec<DropReport> {
        let dropped = Arc::new(DropTally::new());
        let discarded = Arc::new(AtomicU64::new(0));
        let log = ReportLog::default();
        let mut meter = LossMeter::new(
            dropped.clone(),
            discarded,
            Some(log.observer()),
            Duration::ZERO, // every check is due — the cadence is not what is under test
        );
        for burst in bursts {
            // Row by row through the real `record`, on ONE series: the delta discipline is the
            // subject, and driving the real ledger is what puts the per-series half under the same
            // checker as the scalar half.
            for _ in 0..*burst {
                dropped.record(Some(drive_series()));
            }
            meter.report_if_due();
            for _ in 0..silent_windows {
                meter.report_if_due();
            }
        }
        meter.report_final();
        log.reports()
    }

    /// What a TOTAL-based meter would have handed the observer for the same input — the
    /// obvious-but-wrong implementation this whole mechanism exists to not be. It re-states the
    /// running total every window it is asked, so it never falls silent once anything is lost.
    ///
    /// This is the NEGATIVE CONTROL's input, not production code: the checker below must reject it.
    fn total_based_reports(bursts: &[u64], silent_windows: usize) -> Vec<DropReport> {
        let mut out = Vec::new();
        let mut total = 0u64;
        let mut push = |total: u64| {
            out.push(DropReport {
                dropped_delta: total, // ← THE mutation: the total, restated as if it were new
                dropped_total: total,
                // ...and the second half of the same mutation: a scalar report names nobody, which
                // is the state this whole change exists to leave behind.
                dropped_series: Vec::new(),
                discarded_delta: 0,
                discarded_total: 0,
                window: Duration::ZERO,
                final_report: false,
            });
        };
        for burst in bursts {
            total += burst;
            push(total);
            for _ in 0..silent_windows {
                push(total); // still non-zero ⇒ still "reportable" ⇒ one line per window, forever
            }
        }
        push(total); // the closing report re-states it one last time
        out
    }

    /// The delta discipline, as a CHECKER rather than a pile of inline asserts, so the exact same
    /// judgement can be pointed at the real meter and at the broken one and be seen to disagree.
    ///
    /// `Err` describes the violation; `Ok` means: one report per burst, in order, each carrying
    /// that burst's own delta and the running total at the time — and nothing else at all.
    fn check_delta_discipline(reports: &[DropReport], bursts: &[u64]) -> Result<(), String> {
        if let Some(empty) = reports.iter().find(|r| r.lost_delta() == 0) {
            return Err(format!("an empty report was emitted: {empty:?}"));
        }
        if reports.len() != bursts.len() {
            return Err(format!(
                "expected exactly one report per burst ({}), got {} — a report was re-emitted for \
                 a window in which nothing new was lost",
                bursts.len(),
                reports.len()
            ));
        }
        let mut running = 0u64;
        for (i, (report, burst)) in reports.iter().zip(bursts).enumerate() {
            running += burst;
            if report.dropped_delta != *burst {
                return Err(format!(
                    "report {i}: dropped_delta {} is not the {burst} rows lost in THAT window \
                     (a total-based report would say {running})",
                    report.dropped_delta
                ));
            }
            if report.dropped_total != running {
                return Err(format!(
                    "report {i}: dropped_total {} is not the running total {running}",
                    report.dropped_total
                ));
            }
            // The named series must ACCOUNT for the same delta. `drive_meter` charges every burst
            // to one series, so this is exact — and it is what fails on a report that counts rows
            // it cannot attribute (the bare-scalar shape), not merely on a wrong number.
            let named: u64 = report.dropped_series.iter().map(|s| s.delta).sum();
            if named != *burst {
                return Err(format!(
                    "report {i}: the named series account for {named} of the {burst} rows lost in \
                     that window — a drop nothing can name is a gap nobody can find: {:?}",
                    report.dropped_series
                ));
            }
        }
        Ok(())
    }

    /// THE property, gated where it actually lives: [`LossMeter`] reports each non-zero DELTA
    /// exactly once and then stays silent, however many windows pass, however non-zero the
    /// CUMULATIVE total remains.
    ///
    /// Deterministic by construction — no store, no writer thread, no sleeping — because none of
    /// those participate in the decision. The end-to-end test below proves the WIRING (a real burst
    /// through a real writer thread reaches a real observer); this proves the RULE.
    #[test]
    fn the_meter_reports_each_delta_once_and_then_falls_silent() {
        let bursts = [7_u64, 5, 1];
        // 20 windows of nothing-new between bursts: a total-based meter would emit 20 lines each.
        let reports = drive_meter(&bursts, 20);
        check_delta_discipline(&reports, &bursts).expect("the real meter must satisfy this");

        // The deltas partition the total exactly: nothing double-counted, nothing missed.
        let summed: u64 = reports.iter().map(|r| r.dropped_delta).sum();
        assert_eq!(summed, bursts.iter().sum::<u64>(), "the deltas must sum to the total");
        // And the discriminator, stated outright: after the first burst a delta is NOT the total.
        assert_ne!(
            reports[1].dropped_delta, reports[1].dropped_total,
            "a delta that equals the running total is the total-based bug wearing a delta's name"
        );
        // Nothing was lost to a failed flush here, so that axis stays silent throughout.
        assert!(reports.iter().all(|r| r.discarded_delta == 0 && r.discarded_total == 0));
    }

    /// NEGATIVE CONTROL for the test above. The checks it uses are only worth running if they can
    /// FAIL, so point the very same checker at what a total-based meter would have produced for the
    /// very same input and require the opposite verdict.
    ///
    /// Without this, softening `check_delta_discipline` (or the meter) leaves a test that passes
    /// whether or not the delta discipline still holds — which is the failure mode of every
    /// threshold quietly raised past anything a run can reach.
    #[test]
    fn the_delta_checks_reject_a_total_based_meter() {
        let bursts = [7_u64, 5, 1];
        let broken = total_based_reports(&bursts, 20);
        let verdict = check_delta_discipline(&broken, &bursts);
        assert!(
            verdict.is_err(),
            "the checker ACCEPTED a total-based meter — it proves nothing: {broken:?}"
        );
        // ...and it accepts the real one, from the identical input. One checker, two verdicts.
        assert!(check_delta_discipline(&drive_meter(&bursts, 20), &bursts).is_ok());
    }

    /// The closing report is emitted OFF cadence (that is its whole point — a burst in the final
    /// window must not be swallowed by the shutdown), but it is still a delta: with everything
    /// already reported it says nothing at all.
    #[test]
    fn the_closing_report_carries_only_what_was_not_yet_reported() {
        // Nothing reported yet ⇒ the closing report carries the whole burst.
        let only_final = {
            let log = ReportLog::default();
            let mut meter = LossMeter::new(
                tally_of(9),
                Arc::new(AtomicU64::new(0)),
                Some(log.observer()),
                Duration::from_secs(3_600), // never due on cadence — only `report_final` fires
            );
            meter.report_if_due();
            meter.report_final();
            log.reports()
        };
        assert_eq!(only_final.len(), 1, "the final window's loss is never swallowed");
        assert!(only_final[0].final_report);
        assert_eq!(only_final[0].dropped_delta, 9);

        // Already reported ⇒ the closing report is silent. `drive_meter` ends with `report_final`,
        // so the exact-count check above already proves this; stated here as its own case because
        // it is the load-immune half of the end-to-end test below.
        let bursts = [4_u64];
        assert_eq!(
            drive_meter(&bursts, 0).len(),
            1,
            "a closing report with a zero delta is silent"
        );
    }

    /// THE regression test for the reported shape, end to end: a burst of drops made by a real
    /// `RecorderSink` is announced through a real writer thread, and then — while the CUMULATIVE
    /// total stays non-zero forever — reporting falls SILENT again.
    ///
    /// ⚠ **This test waits on CONDITIONS, never on a fixed sleep, and that is the fix for a real
    /// flake** (it failed on the CI box in a full-roster run, then went 10/10 green in isolation). It
    /// used to `thread::sleep(300ms)` and then assert the burst had been reported. But a report's
    /// latency is not set by `drop_report_every` (50ms here) — it is set by when the writer thread
    /// next gets between store commits, and a commit costs ~30-39ms idle and far more under load.
    /// MEASURED on the CI box: the report lands ~80ms after the burst when idle, but p90 176ms / max
    /// 350ms under a 4-way parallel test load and max 1,305ms under an 8-way one, so the 300ms
    /// budget was ~1.7x the p90 and NEGATIVE at the tail. Under a deliberate 4-way load the old
    /// shape failed **12 of 60 runs**, always at "the drop burst was never reported".
    ///
    /// It is a fidelity bug, not a lost report: across 260 instrumented runs the report arrived
    /// late 6 times and was lost **zero** times (the delta accumulates and the next report carries
    /// it; teardown always emits a closing one). That is what makes a bounded wait the right tool —
    /// see [`REPORT_DEADLINE`] for why the opposite finding would forbid one.
    ///
    /// **The silence half is gated causally, not by a clock.** A quiet sleep can only produce a
    /// false PASS (a stalled writer emits nothing, which is what the assertion wants to see), so it
    /// cannot be the proof. `shutdown()` can: `report_final` emits UNCONDITIONALLY, so a total-based
    /// implementation is guaranteed to hand the observer a fresh non-zero `dropped_delta` there,
    /// and the sum below catches it with no timing involved. `the_meter_reports_each_delta_once_and_then_falls_silent`
    /// gates the periodic windows the same way — by arithmetic.
    #[test]
    fn a_drop_burst_is_reported_once_per_delta_then_falls_silent() {
        let (_dir, store) = open_store();
        let log = ReportLog::default();
        let (sink, handle) =
            RecorderSink::spawn_with_observer(store.clone(), chatty(1), Some(log.observer()))
                .unwrap();

        // ONE burst: flood a capacity-1 channel so a pile of `try_send`s fail.
        for i in 0..20_000i64 {
            sink.quote("polymarket", "TOK", quote(i, i as f64));
        }
        // Every `try_send` above ran on THIS thread, so the counter is final at this line: no other
        // producer exists and the writer never bumps it. That is what lets the wait below be a
        // CONDITION ("has all of it been reported yet?") rather than a guess about elapsed time.
        let dropped = handle.dropped();
        assert!(dropped > 0, "flooding a capacity-1 channel must have dropped rows");

        // Wait for the burst to be FULLY reported. Bounded, polled — never a fixed sleep: see this
        // test's doc for the measurements that killed the 300ms one.
        //
        // Reaching the `break` IS the "it was reported at all" assertion, so there is deliberately
        // no `reports.len() >= 1` line after it: with `dropped > 0` the sum cannot match on an
        // empty log, and an assertion that cannot fail is exactly what this PR is about deleting.
        let started = Instant::now();
        loop {
            let reports = log.reports();
            let summed: u64 = reports.iter().map(|r| r.dropped_delta).sum();
            if summed == dropped {
                break;
            }
            assert!(
                started.elapsed() < REPORT_DEADLINE,
                "the drop burst was never fully reported to the observer: {summed} of {dropped} \
                 rows across {} report(s) after {:?}",
                reports.len(),
                started.elapsed()
            );
            thread::sleep(Duration::from_millis(2));
        }

        // Every emitted report is a real loss, and this burst's losses are all `dropped` ones.
        let reports = log.reports();
        assert!(
            reports.iter().all(|r| r.lost_delta() > 0),
            "an empty report must never be emitted"
        );
        assert!(
            reports.iter().all(|r| r.discarded_delta == 0),
            "channel overflow is a `dropped`, never a `discarded`"
        );
        assert_eq!(
            reports.last().expect("non-empty").dropped_total,
            dropped,
            "each report also carries the running total for context"
        );

        // The burst is over. No new rows are sent, so every subsequent window has a ZERO delta —
        // even though `dropped_total` remains non-zero for the rest of the recorder's life.
        //
        // ⚠ This sleep is CORROBORATION, not proof: a writer stalled through the whole window emits
        // nothing either, so it can only ever produce a false pass. The proof is the shutdown below
        // (causal) and `the_meter_reports_each_delta_once_and_then_falls_silent` (arithmetic).
        thread::sleep(chatty(1).drop_report_every * 4);
        let quiet: u64 = log.reports().iter().map(|r| r.dropped_delta).sum();
        assert_eq!(
            quiet, dropped,
            "a drop must be reported once per DELTA, not re-reported every window while the \
             cumulative total stays non-zero"
        );

        // THE mutation gate, and the one step here with no timing in it at all: `shutdown` joins
        // the writer, and `report_final` emits UNCONDITIONALLY on the way out. A total-based
        // implementation therefore MUST hand the observer another `dropped_delta` of `dropped`
        // right here, doubling this sum; the delta-based one contributes exactly nothing.
        //
        // Summing the deltas (rather than counting reports) is deliberate: a shutdown flush that
        // genuinely failed would emit a real closing report on the `discarded` axis, which is not
        // this test's subject and must not read as a failure of it.
        handle.shutdown();
        let after: u64 = log.reports().iter().map(|r| r.dropped_delta).sum();
        assert_eq!(
            after, dropped,
            "the closing report re-stated a dropped delta that had already been reported — the \
             deltas must partition the total exactly, teardown included"
        );
    }

    /// THE regression test for "a gapped tape can never be NAMED": TWO producers share one bounded
    /// queue, and the report must say WHICH series lost rows, not merely how many rows were lost.
    ///
    /// ⚠ **Two producers is the whole point, and no single-producer test can stand in.** The three
    /// tests that already saturate the bound — `overflow_drops_and_counts`,
    /// `equity_overflow_drops_and_counts` and
    /// `a_drop_burst_is_reported_once_per_delta_then_falls_silent` — flood ONE series from ONE
    /// thread and assert `dropped() > 0`, which a counter that names nobody passes exactly as well
    /// as one that names everybody. `age_flush_fires_under_busy_other_series` IS two-producer, but
    /// runs at the 65,536 default and never reaches the bound at all.
    ///
    /// ⚠ **This is not starvation, and the test does not measure fairness.** While the queue is full
    /// EVERY producer's `try_send` fails; the quiet one simply gets far fewer attempts at a freed
    /// slot, so it loses a much larger SHARE of its own rows — which is the the CI box shape, a Polymarket
    /// L2 stream sharing this queue with a Binance tape. What makes that invisible is that the quiet
    /// series is still receiving SOME rows: `flush_buf_with` names a series only on a PERMANENT
    /// FLUSH FAILURE (none here — asserted), and `liveness`/`SilenceWatch` name one only when it
    /// stops ENTIRELY (it does not). PARTIAL loss is the residual between them, and it is what this
    /// pins.
    ///
    /// Every wait is a CONDITION, never a fixed sleep (this file's established idiom): the loop
    /// sends quiet rows until the observer has NAMED the quiet series, so it exits the instant the
    /// property holds and the deadline can only ever turn a hang into a named failure.
    #[test]
    fn two_producers_overflow_and_the_quiet_series_is_named() {
        let (_dir, store) = open_store();
        let log = ReportLog::default();
        // `channel_cap: 1` + the chatty report cadence: the queue is full essentially always, which
        // is the condition BOTH producers are dropping under.
        let (sink, handle) =
            RecorderSink::spawn_with_observer(store.clone(), chatty(1), Some(log.observer()))
                .unwrap();

        // BUSY: floods `trade` rows continuously, as a live venue tape would. A different KIND and
        // a different venue, so the quiet series can only be found by its own key.
        let busy_sink = sink.clone();
        let busy_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let busy_stop_thread = busy_stop.clone();
        let busy = thread::spawn(move || {
            let mut i = 0i64;
            while !busy_stop_thread.load(Ordering::Relaxed) {
                // Bursts rather than an unbroken spin: 32 rows into a ONE-slot queue already keeps
                // it occupied through the writer's ~30ms commits, and pausing between them keeps a
                // test that shares a CI box with seven others from pinning a core.
                for _ in 0..32 {
                    busy_sink.trade("binance", "BTCUSDT", trade(i, i as f64));
                    i += 1;
                }
                thread::sleep(Duration::from_millis(1));
            }
        });

        // QUIET: a low-rate producer on its own series, sent from THIS thread — so once the loop
        // stops, its per-series count is FINAL (nothing else in this test sends a quote), which is
        // what lets the persisted-row equation below be an equality rather than a bound.
        let quiet = quote_key("polymarket", "QUIET");
        let mut sent_quiet = 0i64;
        let started = Instant::now();
        loop {
            for _ in 0..32 {
                sink.quote("polymarket", "QUIET", quote(sent_quiet, sent_quiet as f64));
                sent_quiet += 1;
            }
            let reports = log.reports();
            if reports.iter().any(|r| names_series(r, &quiet)) {
                break;
            }
            assert!(
                started.elapsed() < REPORT_DEADLINE,
                "the quiet series' drops were never NAMED after {:?} ({sent_quiet} quiet rows \
                 sent; {} rows dropped in total, attributed to [{}])",
                started.elapsed(),
                handle.dropped(),
                handle.dropped_summary()
            );
            thread::sleep(Duration::from_millis(2));
        }
        busy_stop.store(true, Ordering::Relaxed);
        busy.join().unwrap();

        // Both producers have stopped, so every count below is final.
        let by_series = handle.dropped_by_series();
        let dropped_quiet = by_series.get(&quiet).copied().unwrap_or(0);
        assert!(dropped_quiet > 0, "the loop exits only once the quiet series was named");
        assert!(
            by_series.keys().any(|k| k.kind == "trade" && k.symbol == "BTCUSDT"),
            "the busy series shares the queue and loses rows too, so it must be named as well — \
             not only whichever series the test thought to ask about: {by_series:?}"
        );
        let summary = handle.dropped_summary();
        assert!(
            summary.contains("quote/polymarket/QUIET="),
            "the rendered tally must spell the series the way `liveness` keys it: {summary}"
        );

        // WHAT THE QUIET SERIES PERSISTS. Every row that reached the writer flushes on the age
        // cadence (20ms here), so the store converges to exactly what was not dropped. A CONDITION
        // wait again — rows may still be in flight at this line — and coarse, because each poll is
        // a full DataFusion read competing with the writer it is waiting for.
        let settle = Instant::now();
        let want = sent_quiet - dropped_quiet as i64;
        let persisted = loop {
            let got = store.scan_quotes("polymarket", "QUIET", TsRange::all()).unwrap();
            if got.len() as i64 == want {
                break got.len() as i64;
            }
            assert!(
                settle.elapsed() < REPORT_DEADLINE,
                "the quiet series never settled at sent({sent_quiet}) - dropped({dropped_quiet}) \
                 = {want}: {} rows persisted after {:?}, discarded={}",
                got.len(),
                settle.elapsed(),
                handle.discarded()
            );
            thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(handle.discarded(), 0, "no flush failed — every loss here was a drop");
        assert!(
            persisted < sent_quiet,
            "the quiet tape MUST have a hole: {dropped_quiet} of {sent_quiet} rows never made it \
             onto the queue, and a series still persisting {persisted} looks alive from every \
             other vantage point"
        );

        // ...and what the REPORT said about it. `shutdown` joins the writer after emitting the
        // closing report, so by the line after it everything lost has been reported exactly once.
        handle.shutdown();
        let mut reported_quiet = 0u64;
        for report in log.reports() {
            for row in &report.dropped_series {
                if row.series == quiet {
                    reported_quiet += row.delta;
                }
            }
            // The named rows never exceed the scalar: a drop nothing could attribute is still
            // counted, and never counted twice (see `DropReport::dropped_series`).
            let named: u64 = report.dropped_series.iter().map(|s| s.delta).sum();
            assert!(
                named <= report.dropped_delta,
                "a report named {named} rows but claims {} were dropped: {report:?}",
                report.dropped_delta
            );
        }
        assert_eq!(
            reported_quiet, dropped_quiet,
            "the per-series deltas must partition that series' own total exactly, teardown \
             included — the same discipline the scalar deltas are held to"
        );
    }

    /// The other half of the contract: a recorder that loses nothing must be COMPLETELY silent.
    /// This is what makes the first warn above meaningful rather than background noise.
    #[test]
    fn a_healthy_recorder_reports_nothing() {
        let (_dir, store) = open_store();
        let log = ReportLog::default();
        let cfg = RecorderConfig { max_rows: 10, ..chatty(65_536) };
        let (sink, handle) =
            RecorderSink::spawn_with_observer(store.clone(), cfg, Some(log.observer())).unwrap();

        for i in 0..100i64 {
            sink.quote("polymarket", "TOK", quote(i, i as f64));
        }
        thread::sleep(Duration::from_millis(300)); // many report windows pass

        assert_eq!(handle.dropped(), 0, "a roomy channel drops nothing");
        assert_eq!(handle.discarded(), 0, "a healthy store discards nothing");
        assert_eq!(handle.lost(), 0);
        // ...and the keyed half is silent too: no series is named, not even with a zero beside it.
        assert!(handle.dropped_by_series().is_empty(), "nothing lost ⇒ nothing named");
        assert_eq!(handle.dropped_summary(), "none");
        assert!(log.reports().is_empty(), "nothing lost ⇒ no report at all");

        handle.shutdown(); // the CLOSING report must also stay silent
        assert!(log.reports().is_empty(), "a clean teardown emits no closing report either");
    }

    /// A flush that fails permanently DISCARDS its buffer — that is unavoidable, there is nowhere
    /// else to put the rows — but the loss must be counted and reported, not just logged once.
    ///
    /// The wedge: plant a plain FILE where the series directory belongs, so `SeriesLock::acquire`'s
    /// opening `create_dir_all` fails immediately and every `append_quotes` for this series fails
    /// fast and permanently. (Failing FAST also exercises the retry — a slow failure is skipped, see
    /// `append_retry_skips_a_slow_failure`.)
    #[test]
    fn a_permanently_failed_flush_discards_and_counts_the_rows() {
        let (dir, store) = open_store();
        let series = quote_series_dir(dir.path(), "polymarket", "TOK");
        std::fs::create_dir_all(series.parent().expect("series dir has a parent")).unwrap();
        std::fs::write(&series, b"not a directory").unwrap();

        let log = ReportLog::default();
        // max_rows == the row count, so ingesting the last row triggers the doomed flush directly —
        // no shutdown needed, which lets the handle still be read afterwards.
        let cfg = RecorderConfig { max_rows: 3, ..chatty(65_536) };
        let (sink, handle) =
            RecorderSink::spawn_with_observer(store.clone(), cfg, Some(log.observer())).unwrap();

        for i in 0..3i64 {
            sink.quote("polymarket", "TOK", quote(i, i as f64));
        }

        let start = Instant::now();
        while log.count() == 0 {
            assert!(
                start.elapsed() < REPORT_DEADLINE,
                "a discarded flush was never reported (discarded={})",
                handle.discarded()
            );
            thread::sleep(Duration::from_millis(20));
        }

        assert_eq!(handle.discarded(), 3, "every row of the failed buffer is counted lost");
        assert_eq!(handle.dropped(), 0, "these rows reached the writer — they were not dropped");
        assert_eq!(handle.lost(), 3, "`lost` is the one number that answers 'any holes?'");

        let first = log.reports()[0].clone();
        assert_eq!(first.discarded_delta, 3);
        assert_eq!(first.discarded_total, 3);
        assert_eq!(first.dropped_delta, 0);
        // Nothing was lost at the CHANNEL, so nothing is named there — a discarded batch is named
        // by `flush_buf_with`'s own `venue`/`symbol` warn instead. The summary must say so in
        // words rather than render a blank field.
        assert!(first.dropped_series.is_empty(), "no row was dropped at the channel here");
        assert_eq!(first.dropped_series_summary(), "none");
        assert_eq!(first.lost_delta(), 3);
        assert_eq!(first.lost_total(), 3);
        assert!(!first.final_report, "this one came from the periodic cadence");

        handle.shutdown();
    }

    // ---- the flush retry policy (pure — no store needed, so the failure modes are exact) --------

    #[test]
    fn append_retry_does_not_re_run_a_success() {
        let mut calls = 0u32;
        // A long retry delay proves the success path never sleeps: a test that reached the sleep
        // would hang for 30s rather than finish instantly.
        let (res, attempts) = append_with_retry(
            || {
                calls += 1;
                Ok(3)
            },
            Duration::from_secs(30),
            Duration::from_secs(1),
        );
        assert_eq!(res.expect("ok"), 3);
        assert_eq!(attempts, 1);
        assert_eq!(calls, 1, "a successful append must not be repeated");
    }

    #[test]
    fn append_retry_recovers_a_transient_fast_failure() {
        let mut calls = 0u32;
        let (res, attempts) = append_with_retry(
            || {
                calls += 1;
                if calls == 1 { Err(DataError::Io("transient".into())) } else { Ok(7) }
            },
            Duration::ZERO,
            Duration::from_secs(1),
        );
        assert_eq!(res.expect("recovered"), 7, "the retry's rows are NOT lost");
        assert_eq!(attempts, 2);
        assert_eq!(calls, 2, "one failure, one retry, done");
    }

    #[test]
    fn append_retry_gives_up_after_exactly_one_extra_attempt() {
        let mut calls = 0u32;
        let (res, attempts) = append_with_retry(
            || {
                calls += 1;
                Err(DataError::Io("boom".into()))
            },
            Duration::ZERO,
            Duration::from_secs(1),
        );
        assert!(res.is_err(), "a permanent failure still fails");
        assert_eq!(attempts, 2, "bounded: ONE retry, never an unbounded loop");
        assert_eq!(calls, 2, "the append ran exactly twice, then the caller discards");
    }

    #[test]
    fn append_retry_skips_a_slow_failure() {
        // A failure that took `slow_attempt` or longer has already burned the store's own retry
        // budget (`SeriesLock::acquire` spins ~4s on a contended lock). Retrying it would mostly
        // double this single writer thread's stall — and every stalled millisecond is more rows
        // dropped at the channel behind it — so it is deliberately not retried.
        let mut calls = 0u32;
        let (res, attempts) = append_with_retry(
            || {
                calls += 1;
                thread::sleep(Duration::from_millis(30));
                Err(DataError::Io("timeout acquiring series lock".into()))
            },
            Duration::ZERO,
            Duration::from_millis(10), // tiny threshold keeps the test fast
        );
        assert!(res.is_err());
        assert_eq!(attempts, 1, "a slow failure is not retried");
        assert_eq!(calls, 1);
    }
}
