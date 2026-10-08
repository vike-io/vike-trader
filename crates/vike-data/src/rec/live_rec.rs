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
//! as already-committed — a silent no-op (see `commit_rows` in `datafusion_hist/ingest.rs`). Without the
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
//! `vike-alerting` — but `vike-data` may not name it. `vike-core` and the user-study host
//! `vike-user-research` both depend on `vike-data`, and two rows of
//! `crates/vike-ops/tests/architecture/named_run_closure_gate/fences.rs`'s `FENCES` refuse `vike-alerting` anywhere in
//! their normal closures: `LIVE_CORE_FENCE` (its blocking `ureq` POST stays off the latency-gated
//! fold) and `USER_CODE_FENCE`. The edge would also drag `ureq`+rustls into a crate
//! `vike-datahub-client` stays light by depending on. ⚠ This argued a PACKAGE CYCLE until
//! 2026-09-28 — `vike-alerting`'s `core` feature depended on `vike-core` — and the cycle went with
//! that feature on 2026-09-23: the crate names no vike crate now and ranks below this one (15
//! against 20), so cargo and the layer gate would both accept the edge, and those two fences are
//! what refuse it. [`DropObserver`] therefore names no alerting type: it hands out a plain
//! [`DropReport`], and a BINARY bridges it — `vike_alerting`'s `FiredAlert` can be built and
//! delivered without linking the trading core, which is what that crate was split out for. That
//! keeps the workspace rule ("libraries take configuration as parameters; only binaries wire")
//! intact. Wiring the observer where a recorder is mounted is the follow-up — no production caller
//! passes one to `RecorderSink::spawn_with_observer` yet (⚠ this named `vike-app`'s and
//! `vike-tradehub`'s `main.rs`, and neither mounts a recorder any more;
//! `crates/vike-datahub/src/recorder.rs`'s `record` does). `spawn` (no observer) is byte-identical
//! to today, so no existing call site changes.
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
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use vike_model::{
    Bar, BookLevel, BookUpdate, BookUpdateKind, EquitySample, L2Book, QuoteTick, TradeTick,
};

use crate::live::{LiveDataSink, StreamStatus};
use crate::store::datafusion_hist::DataFusionHist;
use crate::store::datafusion_hist::GroupResolver;
use crate::store::hist::HistStore;
use crate::store::series::SeriesId;

mod drops;
mod writer;

pub use drops::{DropObserver, DropReport};
use drops::{DropTally, LossMeter, render_drop_tally};
use writer::{BOOK, DEPTH, EQUITY, QUOTE, TRADE, writer_loop};

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
    /// ⚠ **No non-test caller exists yet** — the one production recorder,
    /// `crates/vike-datahub/src/recorder.rs`'s `record`, calls [`spawn`](Self::spawn) (as
    /// `vike-backfill`'s `poly_reparse` bin did until docs/decisions/0094 deleted it on 2026-09-28),
    /// so a [`DropReport`] currently reaches only the `warn!` line and nothing that pages. This is
    /// the PUSH seam waiting for that wiring;
    /// [`RecorderHandle::dropped_by_series`] is the PULL twin for a binary that would rather read
    /// the tally at teardown than hold a callback, and the datahub's recorder stop path does exactly
    /// that, through [`RecorderHandle::dropped_summary`]. (⚠ This named `vike-app`, `vike-tradehub`
    /// and `vike-recorder` as the callers, and `vike_recorder`'s stop path as the reader, until
    /// 2026-09-28: the recorder daemon merged into the datahub, and the other two roots mount no
    /// recorder any more.)
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
    /// (`vike_model::venues::venue_caps`) and refuse `subscribe_book` outright — so for those venues this
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
        bids: Vec<BookLevel>,
        asks: Vec<BookLevel>,
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
        let now = vike_model::now_ms();
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
/// [`crate::store::hist_sched::MaintenanceScheduler`]).
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

#[cfg(test)]
use writer::{age_scan_due, append_with_retry};

#[path = "live_rec_tests.rs"]
#[cfg(test)]
mod live_rec_tests;
