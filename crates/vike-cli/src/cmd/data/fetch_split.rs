//! **Cutting a long `data hist fetch` window into one request per calendar YEAR** — and the rule
//! for WHEN that is allowed, which is the half that matters.
//!
//! # What it is for
//!
//! `Request::Backfill` is synchronous: one request, one answer, nothing in between, and the client
//! clears its read timeout once the handshake is done
//! (`crates/vike-datahub-client/src/client.rs`'s `arm_request_timeouts`). So a fetch of OANDA's whole
//! 5-second history — two decades, one to two hours a pair — was ONE call that printed nothing until
//! it ended. [`plan`] cuts such a window at UTC year boundaries, and `crate::cmd::data`'s
//! `execute_fetch` sends the pieces one after another on the same connection, printing one line to
//! stderr as each finishes ([`run_pieces`]). It is follow-up 1 of
//! `docs/superpowers/specs/2026-09-30-oanda-history-lane-design.md`.
//!
//! # ⚠ The hazard: the store dedups by COMMIT KEY, never by row
//!
//! Cutting a window is invisible to the store ONLY where the lane that serves it keys what it
//! stores by something the request does not choose. Read from the lanes
//! `crates/vike-datahub/src/backfill.rs`'s `real_backfill_table` dispatches, and the ingest each one
//! calls:
//!
//! * **`Klines`** (the six keyless kline venues) — `crates/vike-backfill/src/klines.rs`'s
//!   `ingest_klines`: ONE commit key per request, over the REQUEST's own `[start, end]`. Asked a
//!   year at a time, it stores every bar under a per-year key that a later whole-window request
//!   does not share, and that request stores the same bars AGAIN. Never split.
//! * **`Funding`** — `crates/vike-backfill/src/funding_rate.rs`'s `backfill_funding_rate`: the same
//!   per-request key (`funding_rate_commit_key`). Never split.
//! * **`TickBars`** (dukascopy) — `crates/vike-backfill/src/venues/dukascopy.rs`'s
//!   `backfill_quotes_then_bars` is chunked on a grid of its own, but only a FULL, SETTLED chunk is
//!   keyed by that grid: a partial chunk is keyed by its own cut, and the recent tail by the
//!   request's raw bounds (a one-chunk window's by a provisional key). Its grid is a whole number of
//!   BARS, which for a step that does not divide a day (`7m`) does not land on a year boundary — so
//!   a cut there would mint two partial chunks no whole-window request makes. Never split.
//! * **`CredentialedKlines`** (OANDA) — `crates/vike-datahub/src/backfill.rs`'s
//!   `credentialed_klines_row` calls `backfill_kline_source_chunked`, i.e.
//!   `crates/vike-backfill/src/klines.rs`'s `ingest_klines_chunked`: the request is rounded OUTWARD
//!   to whole UTC days and every day is stored under the GRID's key `{c0}-{c1}`, whatever request
//!   covered it, and a day whose key is spent is skipped before any fetch. A year boundary is a UTC
//!   midnight, so the pieces cover exactly the days the whole window covers, no day twice. The ONE
//!   lane split.
//!
//! # How this side knows which lane a venue takes — a fact it already links
//!
//! `vike_catalog::history_channels_for`'s `ChannelState::Built(lane)` cells, the rows
//! `vike-cli data source show` renders. No wire field was added: that table's `Built` cells are held
//! equal to the datahub's dispatch table in BOTH directions by
//! `crates/vike-ops/tests/history_channels_gate.rs`'s `built_is_exactly_the_datahubs_lane_table`,
//! and the property this module rests on — the credentialed lane is the day-grid ingest — is
//! pinned beside it by `the_credentialed_lane_is_the_day_grid_ingest`. [`stores_whole_utc_days`] is
//! an EXHAUSTIVE match, so a lane added to the catalog does not compile here until somebody has
//! decided whether it may be split.
//!
//! ⚠ **A venue splits only when EVERY lane its rows claim is a day-grid one.** A venue with both a
//! day-grid lane and a one-shot lane would be split or not by the INTERVAL (the datahub picks the
//! funding lane by the reserved label), and this side would have to restate that rule; refusing to
//! split it at all costs only the progress lines. No such venue exists today.
//!
//! ⚠ **What this does NOT see: the server's own build.** The table is the one this binary was built
//! with. A datahub older than the credentialed lane answers the first piece that it has no collector
//! for the venue, and nothing is written — the same refusal one request would get. The skew that
//! WOULD matter — this binary believing a lane is the day-grid one while the server answers it with
//! a per-request key — needs the datahub to move a venue between ingests, which the lane's design
//! forbids for its own reason (one series takes ONE of the two ingests, or every shared bar is stored
//! twice: `crates/vike-backfill/src/kline_source.rs`'s `backfill_kline_source_chunked`).
//!
//! # The cut
//!
//! * **A window of one year or less is never split.** "One year" is calendar: the same instant one
//!   year later, a 29 February start counting to 1 March. A split exists to report progress, and a
//!   year is what an operator was told to run by hand before this existed.
//! * **The cut points are 1 January 00:00:00 UTC, strictly INSIDE the window.** A piece ends at the
//!   last millisecond before a cut — the wire's windows are inclusive — so no instant is in two
//!   pieces and none in neither. A window ending exactly at a year's first instant keeps that instant
//!   in its last piece rather than gaining a one-millisecond piece of its own.
//! * **The first and last pieces keep the operator's own bounds**, so the outer edges are exactly
//!   what one request would have sent.
//! * **`--days N` is resolved to a window first**, by `crate::cmd::data`'s `fetch_window_ms`, and
//!   then split like any other.
//!
//! # Failure and resume
//!
//! [`run_pieces`] stops at the first piece that fails and names it, the pieces before it and the
//! rows they wrote, which stay stored. Re-running the same command resumes, because the lane skips
//! every day whose key is spent — the property `crates/vike-cli/tests/data_cli.rs` proves end to end
//! through an in-process datahub rather than asserting from this paragraph.

use std::time::{Duration, Instant};

use vike_catalog::{ChannelState, HistoryLane, history_channels_for};
use vike_datahub_client::BackfillDone;
use vike_model::{MS_PER_DAY, civil_from_days, days_from_civil, epoch_ms_to_utc_date};

/// Whether `lane` stores whole UTC days under the day GRID's commit keys, independent of the
/// request — the one property that makes cutting a request invisible to the store. See this
/// module's doc for each lane's ingest and key.
///
/// ⚠ EXHAUSTIVE on purpose: a lane added to `vike_catalog::HistoryLane` stops this compiling until
/// somebody has read its ingest and answered.
///
/// This module's tests live in `crate::cmd::data`'s `data_tests` (its `fetch_split_cases` module),
/// not in a test file of their own: `crates/vike-ops/tests/compile_time_path_gate.rs` ratchets the
/// number of `#[cfg(test)] mod NAME;` files, and a new one would have had to raise that shared
/// ceiling.
pub(super) fn stores_whole_utc_days(lane: HistoryLane) -> bool {
    match lane {
        HistoryLane::CredentialedKlines => true,
        HistoryLane::Klines | HistoryLane::TickBars | HistoryLane::Funding => false,
    }
}

/// Whether a fetch at `venue` may be cut into per-year requests: every lane the history table says
/// the datahub builds for it stores whole UTC days, and there is at least one. An unknown venue
/// string declares nothing and is never split — its single request is refused or answered exactly
/// as before.
pub(super) fn splits_by_year(venue: &str) -> bool {
    let mut lanes = history_channels_for(venue).iter().filter_map(|row| match row.state {
        ChannelState::Built(lane) => Some(lane),
        ChannelState::Designed(_) => None,
    });
    let Some(first) = lanes.next() else { return false };
    stores_whole_utc_days(first) && lanes.all(stores_whole_utc_days)
}

/// The calendar year holding `ms`, UTC.
fn year_of(ms: i64) -> i64 {
    civil_from_days(ms.div_euclid(MS_PER_DAY)).0
}

/// 1 January 00:00:00 UTC of `year`, in epoch-ms; `None` past the edge of `i64`.
fn year_start(year: i64) -> Option<i64> {
    days_from_civil(year, 1, 1).checked_mul(MS_PER_DAY)
}

/// The same instant one calendar year after `ms`. A 29 February maps to 1 March, which is what
/// `days_from_civil` answers for a 29 February that does not exist. `None` past the edge of `i64`.
fn one_year_after(ms: i64) -> Option<i64> {
    let (y, m, d) = civil_from_days(ms.div_euclid(MS_PER_DAY));
    days_from_civil(y.checked_add(1)?, m, d)
        .checked_mul(MS_PER_DAY)?
        .checked_add(ms.rem_euclid(MS_PER_DAY))
}

/// How a fetch's `[start, end]` goes on the wire: one request, or one per calendar year.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Plan {
    start: i64,
    end: i64,
    /// Cut at the year boundaries inside the window. Only ever `true` for a window LONGER than a
    /// year, so a split plan always has at least two pieces.
    by_year: bool,
}

/// The plan for a fetch of `[start, end]` (inclusive epoch-ms, `start <= end`) at `venue`: per
/// year when [`splits_by_year`] says the venue's lane allows it and the window is longer than one
/// year, otherwise the one request every fetch sent before this module existed.
pub(super) fn plan(venue: &str, start: i64, end: i64) -> Plan {
    if splits_by_year(venue) { Plan::by_year(start, end) } else { Plan::whole(start, end) }
}

impl Plan {
    /// The one request, `[start, end]`, unchanged.
    pub(super) fn whole(start: i64, end: i64) -> Self {
        Self { start, end, by_year: false }
    }

    /// The year cut, whatever the venue — the arithmetic alone. A window of one year or less (see
    /// the module doc) stays one piece.
    pub(super) fn by_year(start: i64, end: i64) -> Self {
        let longer = one_year_after(start).is_some_and(|a| end > a);
        Self { start, end, by_year: longer }
    }

    /// The whole window, as asked.
    pub(super) fn window(&self) -> (i64, i64) {
        (self.start, self.end)
    }

    /// Whether this plan sends more than one request.
    pub(super) fn is_split(&self) -> bool {
        self.by_year
    }

    /// How many requests [`Plan::pieces`] yields, computed without walking them: one per year
    /// boundary strictly inside the window, plus one.
    pub(super) fn count(&self) -> u64 {
        if !self.by_year {
            return 1;
        }
        let (first, last) = (year_of(self.start), year_of(self.end));
        // Every year after the first starts inside the window, except the window's own last year
        // when the window ENDS on its first instant.
        let on_the_edge = year_start(last) == Some(self.end);
        (last - first) as u64 - u64::from(on_the_edge) + 1
    }

    /// The pieces, in order, as inclusive `[from, to]` epoch-ms windows. Lazily: a window of any
    /// length costs one pair at a time.
    pub(super) fn pieces(&self) -> impl Iterator<Item = (i64, i64)> {
        let (end, by_year) = (self.end, self.by_year);
        let mut from = Some(self.start);
        let mut next_year = year_of(self.start).saturating_add(1);
        std::iter::from_fn(move || {
            let at = from?;
            // A cut strictly inside the window; the year after the start's own always begins after
            // the start, so only the END side needs checking.
            let cut = if by_year { year_start(next_year).filter(|&c| c < end) } else { None };
            match cut {
                Some(c) => {
                    from = Some(c);
                    next_year = next_year.saturating_add(1);
                    Some((at, c - 1))
                }
                None => {
                    from = None;
                    Some((at, end))
                }
            }
        })
    }
}

/// One piece that finished: its window, the datahub's answer for it, and how long it took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PieceDone {
    pub(super) from: i64,
    pub(super) to: i64,
    pub(super) done: BackfillDone,
    pub(super) elapsed: Duration,
}

/// The first piece that failed, and what the run had stored before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PieceFailed {
    /// 1-based.
    pub(super) index: u64,
    pub(super) count: u64,
    pub(super) from: i64,
    pub(super) to: i64,
    /// Rows the pieces BEFORE this one wrote. The failed piece's own count is in `error`: the lane
    /// names the chunk it stopped at and the bars it had written by then.
    pub(super) rows_before: u64,
    /// The request's own failure text, as the caller rendered it.
    pub(super) error: String,
}

impl PieceFailed {
    /// The sentence the run ends on — what failed, what stays stored, and how to resume.
    pub(super) fn message(&self, venue: &str) -> String {
        let before = match self.index - 1 {
            0 => "it was the first, so this run wrote nothing before it".to_string(),
            1 => format!("the piece before it wrote {} rows, which stay stored", self.rows_before),
            n => format!(
                "the {n} pieces before it wrote {} rows, which stay stored",
                self.rows_before
            ),
        };
        format!(
            "piece {} of {} [{} .. {}] failed; {before}. Re-run the same command to resume: {venue}'s \
             history lane skips every UTC day it has already stored, so only what is missing is \
             fetched again.\n  {}",
            self.index,
            self.count,
            epoch_ms_to_utc_date(self.from),
            epoch_ms_to_utc_date(self.to),
            self.error
        )
    }
}

/// What a whole split run stored — merged the way ONE request's answer reads: the rows summed, the
/// earliest first and the latest last of the pieces that held any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SplitDone {
    pub(super) rows_written: u64,
    pub(super) first_ts: Option<i64>,
    pub(super) last_ts: Option<i64>,
    pub(super) pieces: Vec<PieceDone>,
}

/// The line printed before the first piece is sent.
pub(super) fn header_line(spec: &str, venue: &str, plan: &Plan) -> String {
    let (start, end) = plan.window();
    format!(
        "fetching {spec} [{} .. {}] as {} requests, one per calendar year: {venue}'s history lane \
         stores whole UTC days under keys of their own, so the pieces store exactly what one \
         request would. One line as each finishes:",
        epoch_ms_to_utc_date(start),
        epoch_ms_to_utc_date(end),
        plan.count()
    )
}

/// The line printed as piece `index` (1-based) of `count` finishes.
pub(super) fn progress_line(index: u64, count: u64, piece: &PieceDone) -> String {
    format!(
        "  {index}/{count} [{} .. {}]: {} rows written in {}",
        epoch_ms_to_utc_date(piece.from),
        epoch_ms_to_utc_date(piece.to),
        piece.done.rows_written,
        elapsed(piece.elapsed)
    )
}

/// A duration the way an operator reads a long fetch: tenths of a second under a minute, then whole
/// minutes and seconds, then hours and minutes.
pub(super) fn elapsed(d: Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0..60 => format!("{:.1}s", d.as_secs_f64()),
        60..3600 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

/// Send every piece of `plan` through `request`, in order, calling `progress` with `header` before
/// the first and with one line as each finishes. Stops at the FIRST failure — the pieces after it
/// are never sent — and answers what the run stored before it.
///
/// `request` is the wire call (`DatahubClient::backfill` over the run's one connection, its error
/// already rendered), a parameter so the stop rule is testable without a socket.
pub(super) fn run_pieces(
    plan: &Plan,
    header: &str,
    mut request: impl FnMut(i64, i64) -> Result<BackfillDone, String>,
    mut progress: impl FnMut(&str),
) -> Result<SplitDone, PieceFailed> {
    let count = plan.count();
    progress(header);
    let mut out = SplitDone { rows_written: 0, first_ts: None, last_ts: None, pieces: Vec::new() };
    for (n, (from, to)) in plan.pieces().enumerate() {
        let index = n as u64 + 1;
        let started = Instant::now();
        let done = match request(from, to) {
            Ok(done) => done,
            Err(error) => {
                return Err(PieceFailed {
                    index,
                    count,
                    from,
                    to,
                    rows_before: out.rows_written,
                    error,
                });
            }
        };
        let piece = PieceDone { from, to, done, elapsed: started.elapsed() };
        progress(&progress_line(index, count, &piece));
        out.rows_written += piece.done.rows_written;
        out.first_ts = match (out.first_ts, piece.done.first_ts) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        out.last_ts = match (out.last_ts, piece.done.last_ts) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        out.pieces.push(piece);
    }
    Ok(out)
}
