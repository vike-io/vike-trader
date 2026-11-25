//! The RANGE-read verb family — `LoadBars`, the thirteen scans (`ScanQuotes`, `ScanTrades`,
//! `ScanBookUpdates`, `ScanDepth`, `ScanCohort`, `ScanPerpMetrics`, `ScanTexts`, `ScanChain`,
//! `ScanFinancialFacts`, `ScanInstitutionalHoldings`, `ScanTextScores`, `ScanForecasts`, `ScanEquity`) and the two
//! execution-history reads (`ScanExecFills`, `ScanExecOrders`), all through [`range_verb`]: a store
//! read that stops at what one reply can carry, a whole-`ts` row cap, and an exact byte fit to the frame. `handle_request`
//! (the parent module) holds the arms and passes each its [`RangeVerb`] vocabulary and ceiling; the
//! ceilings themselves (`LOAD_BARS_CEILING` and its siblings, [`ReadCeilings`]) are public constants
//! of the parent module and stay there.

use super::*;
use vike_datahub_client::proto::Request;
use vike_datahub_client::refusals::DatahubRefusal;

/// Cap `rows` to about `limit`, cutting ONLY on a whole-`ts` boundary.
///
/// ⚠ **The softness is the point.** A paging client continues from `last_timestamp + 1` — the reply
/// carries no cursor, for the wire-compatibility reason [`FEATURE_SCAN_LIMIT`] argues — so a page
/// cut mid-`ts` would strand the rest of that timestamp's rows on the far side of the client's own
/// next bound. They would vanish with no error and no gap: a short answer shaped exactly like the
/// truth. `scan_book_updates` makes it worse still, since it REGROUPS on `(ts, seq)` and a mid-`ts`
/// cut splits one logical event into two partial ones.
///
/// Three cases, and the third is the one that is easy to get wrong:
///
/// * the cap falls exactly on a group boundary — take `limit` rows;
/// * a group STRADDLES the cap — walk back to that group's first row and stop before it, so the
///   page is short of the cap rather than over it;
/// * the straddling group is the FIRST one — take it WHOLE, over the cap. Returning nothing here
///   would be correct by the letter and useless: a client would ask the identical question
///   forever. Progress outranks the cap, and a single `ts` larger than a page is the one shape
///   this wire cannot bound.
///
/// Every row family reaching this function is ts-ascending, which is what makes the walk a local
/// one; `vike_data::store::store_kind`'s codecs sort on `ts` first for every kind.
fn cap_to_whole_timestamp<T>(
    mut rows: Vec<T>,
    limit: Option<u32>,
    ts_of: impl Fn(&T) -> i64,
) -> Vec<T> {
    let Some(limit) = limit.map(|l| l as usize) else { return rows };
    if limit == 0 || rows.len() <= limit {
        return rows;
    }
    let straddling = ts_of(&rows[limit - 1]);
    if ts_of(&rows[limit]) != straddling {
        rows.truncate(limit);
        return rows;
    }
    let mut cut = limit - 1;
    while cut > 0 && ts_of(&rows[cut - 1]) == straddling {
        cut -= 1;
    }
    if cut == 0 {
        let mut end = limit;
        while end < rows.len() && ts_of(&rows[end]) == straddling {
            end += 1;
        }
        cut = end;
    }
    rows.truncate(cut);
    rows
}

/// What a range verb is called, what its rows are, and what its ceiling is named — the vocabulary
/// of its refusal, one per verb [`range_verb`] serves.
pub(super) struct RangeVerb {
    verb: &'static str,
    unit: &'static str,
    ceiling_name: &'static str,
}

pub(super) const LOAD_BARS: RangeVerb =
    RangeVerb { verb: "LoadBars", unit: "bars", ceiling_name: "LOAD_BARS_CEILING" };
pub(super) const SCAN_QUOTES: RangeVerb =
    RangeVerb { verb: "ScanQuotes", unit: "quotes", ceiling_name: "SCAN_QUOTES_CEILING" };
pub(super) const SCAN_TRADES: RangeVerb =
    RangeVerb { verb: "ScanTrades", unit: "trades", ceiling_name: "SCAN_TRADES_CEILING" };
pub(super) const SCAN_BOOK_UPDATES: RangeVerb = RangeVerb {
    verb: "ScanBookUpdates",
    unit: "book levels",
    ceiling_name: "SCAN_BOOK_LEVELS_CEILING",
};
pub(super) const SCAN_DEPTH: RangeVerb =
    RangeVerb { verb: "ScanDepth", unit: "book levels", ceiling_name: "SCAN_BOOK_LEVELS_CEILING" };
pub(super) const SCAN_COHORT: RangeVerb =
    RangeVerb { verb: "ScanCohort", unit: "cohort rows", ceiling_name: "SCAN_COHORT_CEILING" };
pub(super) const SCAN_PERP_METRICS: RangeVerb = RangeVerb {
    verb: "ScanPerpMetrics",
    unit: "perp-metric rows",
    ceiling_name: "SCAN_PERP_METRICS_CEILING",
};
pub(super) const SCAN_CHAIN: RangeVerb =
    RangeVerb { verb: "ScanChain", unit: "option-chain rows", ceiling_name: "SCAN_CHAIN_CEILING" };
pub(super) const SCAN_EQUITY: RangeVerb =
    RangeVerb { verb: "ScanEquity", unit: "equity samples", ceiling_name: "SCAN_EQUITY_CEILING" };
pub(super) const SCAN_TEXTS: RangeVerb =
    RangeVerb { verb: "ScanTexts", unit: "texts", ceiling_name: "SCAN_TEXTS_CEILING" };
pub(super) const SCAN_FINANCIAL_FACTS: RangeVerb = RangeVerb {
    verb: "ScanFinancialFacts",
    unit: "reported values",
    ceiling_name: "SCAN_FINANCIAL_FACTS_CEILING",
};
pub(super) const SCAN_INSTITUTIONAL_HOLDINGS: RangeVerb = RangeVerb {
    verb: "ScanInstitutionalHoldings",
    unit: "13F lines",
    ceiling_name: "SCAN_INSTITUTIONAL_HOLDINGS_CEILING",
};
pub(super) const SCAN_TEXT_SCORES: RangeVerb = RangeVerb {
    verb: "ScanTextScores",
    unit: "text scores",
    ceiling_name: "SCAN_TEXT_SCORES_CEILING",
};
/// `ScanExecFills` — a windowless request (every client older than `scan_execution_fills_range` sends one)
/// is [`range_verb`]'s no-`limit` arm over the whole series: all of it, or refused by name above
/// [`SCAN_EXECUTION_FILLS_CEILING`].
pub(super) const SCAN_EXECUTION_FILLS: RangeVerb =
    RangeVerb { verb: "ScanExecFills", unit: "fills", ceiling_name: "SCAN_EXEC_FILLS_CEILING" };
pub(super) const SCAN_EXECUTION_ORDERS: RangeVerb = RangeVerb {
    verb: "ScanExecOrders",
    unit: "order snapshots",
    ceiling_name: "SCAN_EXEC_ORDERS_CEILING",
};
/// `StrategyFundingHistory`: `LoadBars`' read of the one `interval=funding` series, on the strategy lane's key.
pub(super) const STRATEGY_FUNDING_HISTORY: RangeVerb = RangeVerb {
    verb: "StrategyFundingHistory",
    unit: "funding settlements",
    ceiling_name: "LOAD_BARS_CEILING",
};
/// `StrategyInputBars`: `LoadBars`' read of one bar series a live strategy's input names, on the strategy lane's key.
pub(super) const STRATEGY_INPUT_BARS: RangeVerb =
    RangeVerb { verb: "StrategyInputBars", unit: "bars", ceiling_name: "LOAD_BARS_CEILING" };
/// `StrategyForecasts`: the `kind=exec_forecast` series of one `(venue, symbol)`, on the strategy lane's key.
pub(super) const STRATEGY_FORECASTS: RangeVerb = RangeVerb {
    verb: "StrategyForecasts",
    unit: "forecasts",
    ceiling_name: "SCAN_FORECASTS_CEILING",
};
/// `ScanForecasts`: the same series and ceiling as [`STRATEGY_FORECASTS`], on the data plane's Observe key.
pub(super) const SCAN_FORECASTS: RangeVerb =
    RangeVerb { verb: "ScanForecasts", unit: "forecasts", ceiling_name: "SCAN_FORECASTS_CEILING" };

/// One range verb — `LoadBars`, the eight scans and the two exec reads — answered from a read that stops at what the
/// reply can carry. `read(n)` is the verb's bounded store read for `n` rows: a COMPLETE PREFIX of
/// the range holding at least `n` stored rows unless it is the whole range (`HistoryStore`'s
/// `load_bars_head`, or a `scan_*_capped` with `Some(n)`). `stored_rows` counts what the store's `n`
/// counts — rows, except for the book kinds, whose store counts one row per LEVEL
/// ([`book_stored_rows`]).
///
/// ⚠ **The store is asked for a HEAD, never for the range.** Every one of these arms used to read
/// the client's whole range — `LoadBars` through `load_bars`, the three research scans through their
/// unbudgeted reads on EVERY page, the four tick scans whenever no `limit` was sent — and then
/// [`cap_to_whole_timestamp`], so the frame was bounded and the allocation was whatever the client asked
/// for: gigabytes for one request over a long `5s` series, and, through `HistoryStoreDatahub`'s pager,
/// the rest of the range again for every page. Both arms below need only the head:
///
/// * **With a `limit` `L`**, the store is asked for `min(L, ceiling)` rows and the reply is
///   [`cap_to_whole_timestamp`] of them — BYTE-IDENTICAL to the old reply whenever `L` is within the
///   ceiling, because a complete prefix of at least `L` rows holds every row the cap's decision
///   looks at (`docs/superpowers/specs/2026-10-01-loadbars-bounded-read-design.md`'s section 2
///   carries the three cases). A `limit` above the ceiling is CLAMPED to it: a page shorter than
///   its `limit` is already legal under the soft cap, and every reply longer than the ceiling
///   overran the frame anyway (see [`LOAD_BARS_CEILING`] and its siblings).
/// * **With no `limit`** — and with `Some(0)`, which [`cap_to_whole_timestamp`] has always read as "no
///   cap" — the store is asked for `ceiling + 1` rows. More than `ceiling` back means the range
///   holds more than one reply can carry, and it is REFUSED BY NAME rather than read, clamped in
///   silence, or sent to fail at `write_frame`; fewer means the answer IS the whole range, which the
///   contract's at-least half guarantees, so the reply is exactly the old one.
///
/// The cap stays HERE, in the server, for the reason it exists at all: a store promises only that
/// its prefix never splits a timestamp, and cutting on a whole-`ts` boundary is the wire's rule.
/// ⚠ For the book kinds the cap counts EVENTS while the read counts levels, exactly as it has since
/// budgets landed; a page of events at least `L` long is not promised there, only a complete one.
///
/// ⚠ **…and then the BYTES are cut too, because a row ceiling is not a frame.** Each ceiling is the
/// frame divided by its kind's SHORTEST row, so a reply under it of wider rows can still overrun
/// `frame_bytes` — a 30-day window of `5s` bars, 518,400 of them at about 146 bytes, was one — and
/// before this step that reply was read, serialised and dropped at `write_frame`. [`fit_to_frame`]
/// counts every row's exact JSON and cuts on a whole `ts`: with a `limit`, the shorter page it
/// answers is as legal as any soft-capped page; with none, a reply over the frame is REFUSED BY
/// NAME. Neither arm ever answers an EMPTY page in place of rows (`fit_to_frame`'s doc says why).
/// Every reply that fits is untouched, so byte-identical to before this step existed.
pub(super) fn range_verb<T: serde::Serialize>(
    what: &RangeVerb,
    series: &str,
    range: TimestampRange,
    limit: Option<u32>,
    ceiling: usize,
    frame_bytes: usize,
    read: impl FnOnce(usize) -> Result<Vec<T>, vike_data::HistoryStoreError>,
    stored_rows: impl Fn(&[T]) -> usize,
    ts_of: impl Fn(&T) -> i64,
    wrap: fn(Vec<T>) -> Response,
) -> Response {
    // The body `write_frame` builds: the envelope around no rows, each row's JSON and a comma before
    // every row but the first.
    let envelope = json_len(&wrap(Vec::new()));
    let json_row = |previous: Option<&T>, row: &T| {
        json_len(row).saturating_add(usize::from(previous.is_some()))
    };
    let frame = FrameBudget { bytes: frame_bytes, envelope };
    match range_rows(what, series, range, limit, ceiling, frame, read, stored_rows, ts_of, json_row)
    {
        Ok(rows) => wrap(rows),
        Err(refusal) => Response::Refused(refusal.into()),
    }
}

/// A reply's frame, as [`range_rows`] fits rows to it: the ceiling `bytes`, and the `envelope`, what the
/// reply's body holds around no rows at all.
pub(super) struct FrameBudget {
    pub(super) bytes: usize,
    pub(super) envelope: usize,
}

/// The rows one range reply carries, or the refusal it answers instead: [`range_verb`]'s two arms over
/// any body `row_len` counts (`row_len(previous, row)` is what `row` adds after `previous`), so the JSON
/// reply and the datahub's columns (`super::route`'s `columns_step` callers, bars and the tick-level scans) cut
/// the same rows by the same rules and refuse in the same words.
pub(super) fn range_rows<T>(
    what: &RangeVerb,
    series: &str,
    range: TimestampRange,
    limit: Option<u32>,
    ceiling: usize,
    frame: FrameBudget,
    read: impl FnOnce(usize) -> Result<Vec<T>, vike_data::HistoryStoreError>,
    stored_rows: impl Fn(&[T]) -> usize,
    ts_of: impl Fn(&T) -> i64,
    row_len: impl Fn(Option<&T>, &T) -> usize,
) -> Result<Vec<T>, DatahubRefusal> {
    let frame_bytes = frame.bytes;
    match limit.filter(|l| *l > 0) {
        Some(limit) => {
            let n = (limit as usize).min(ceiling);
            let rows = read(n).map_err(|e| DatahubRefusal::from(&e))?;
            // `n <= limit`, so it is a `u32` again without loss.
            let rows = cap_to_whole_timestamp(rows, Some(n as u32), &ts_of);
            match fit_to_frame(rows, &frame, &ts_of, &row_len) {
                FrameFit::Whole(rows) | FrameFit::Prefix(rows) => Ok(rows),
                FrameFit::FirstTimestampOver { timestamp: ts } => {
                    Err(first_timestamp_over_frame(what, series, ts, frame_bytes))
                }
            }
        }
        None => {
            let rows = read(ceiling.saturating_add(1)).map_err(|e| DatahubRefusal::from(&e))?;
            if stored_rows(&rows) > ceiling {
                return Err(range_refusal(what, series, range, ceiling));
            }
            match fit_to_frame(rows, &frame, &ts_of, &row_len) {
                FrameFit::Whole(rows) => Ok(rows),
                FrameFit::Prefix(_) | FrameFit::FirstTimestampOver { .. } => {
                    Err(range_over_frame(what, series, range, frame_bytes))
                }
            }
        }
    }
}

/// What [`fit_to_frame`] found.
enum FrameFit<T> {
    /// Every row fits the frame, and the rows are handed back untouched.
    Whole(Vec<T>),
    /// They did not all fit: the longest whole-`ts` prefix that does — never empty.
    Prefix(Vec<T>),
    /// The rows of the FIRST timestamp alone take more than the frame, so no page can carry them.
    FirstTimestampOver { timestamp: i64 },
}

/// Cut `rows` (ts-ascending) to the longest whole-`ts` prefix whose reply fits `frame.bytes`,
/// counting EXACTLY rather than estimating: `frame.envelope` plus what `row_len` says each row adds.
/// For JSON ([`range_verb`]) that is each row's JSON, written into a `ByteCount` that allocates
/// nothing, plus one separating comma per row after the first, plus the variant's envelope around no
/// rows: the body `write_frame` builds, byte for byte, so "fits here" and "passes `write_frame`" are
/// the same statement, at one extra serialisation pass of CPU and no memory. For the columns it
/// is `vike_datahub_client::proto::columns_row_len`, arithmetic on the row.
///
/// ⚠ **It NEVER answers an empty prefix in place of rows.** A paging client stops on its first
/// EMPTY page (`HistoryStoreDatahub`'s `paged`), so an empty answer from a range that goes on reads as
/// its end and the rest is lost with no error — the loss
/// `docs/superpowers/specs/2026-10-01-tick-scan-paging-silent-loss-design.md` fixed, made again by a
/// different cap. When the first timestamp's rows alone overrun the frame the answer is
/// [`FrameFit::FirstTimestampOver`], which every caller turns into a NAMED error.
///
/// No row width is assumed anywhere, so there is no "skip the count when every row is narrow"
/// shortcut: even a bar carries an `Option<String>` symbol, so no kind here has a widest row.
fn fit_to_frame<T>(
    mut rows: Vec<T>,
    frame: &FrameBudget,
    ts_of: impl Fn(&T) -> i64,
    row_len: impl Fn(Option<&T>, &T) -> usize,
) -> FrameFit<T> {
    let frame_bytes = frame.bytes;
    let mut total = frame.envelope;
    // `rows[..boundary]` is the longest whole-`ts` prefix known to fit: it is moved only at a `ts`
    // change, and only while every row before it has been counted without passing the frame.
    let mut boundary = 0usize;
    for i in 0..rows.len() {
        if i > 0 && ts_of(&rows[i]) != ts_of(&rows[i - 1]) {
            boundary = i;
        }
        total = total.saturating_add(row_len(i.checked_sub(1).map(|p| &rows[p]), &rows[i]));
        if total > frame_bytes {
            if boundary == 0 {
                return FrameFit::FirstTimestampOver { timestamp: ts_of(&rows[0]) };
            }
            rows.truncate(boundary);
            return FrameFit::Prefix(rows);
        }
    }
    FrameFit::Whole(rows)
}

/// The exact length of `value`'s compact JSON — what `serde_json::to_vec` would produce, and so
/// what it contributes to a frame body (`vike_hub_protocol::frame::json_body_len`). A value serde cannot
/// write (none of the row types here can fail) counts as larger than any frame, which refuses rather
/// than sends it.
fn json_len<T: serde::Serialize + ?Sized>(value: &T) -> usize {
    vike_hub_protocol::frame::json_body_len(value).unwrap_or(usize::MAX)
}

/// The named refusal for a reply with no `limit` whose rows number no more than their ceiling but
/// whose JSON passes the frame — the case a row ceiling derived from the SHORTEST row cannot see.
fn range_over_frame(
    what: &RangeVerb,
    series: &str,
    range: TimestampRange,
    frame_bytes: usize,
) -> DatahubRefusal {
    DatahubRefusal::RangeOverFrame {
        verb: what.verb,
        series: series.to_string(),
        range: bounds(range),
        unit: what.unit,
        frame_bytes,
        ceiling_name: what.ceiling_name,
    }
}

/// A refused range as its refusal names it: `[<start>, <end>]`, each end its epoch-ms or `open`.
fn bounds(range: TimestampRange) -> String {
    let bound = |b: Option<i64>| b.map_or_else(|| "open".to_string(), |t| t.to_string());
    format!("[{}, {}]", bound(range.start), bound(range.end))
}

/// The named error for a page whose FIRST timestamp's rows alone pass the frame — answered in place
/// of an EMPTY page, which a pager would read as the end of the range.
fn first_timestamp_over_frame(
    what: &RangeVerb,
    series: &str,
    ts: i64,
    frame_bytes: usize,
) -> DatahubRefusal {
    DatahubRefusal::FirstTimestampOverFrame {
        verb: what.verb,
        series: series.to_string(),
        unit: what.unit,
        ts,
        frame_bytes,
    }
}

/// The rows a book answer stands for in the STORE's count — one per level, and one placeholder for
/// an event with none (`vike_data`'s book codec, `book_rows`) — so a no-`limit` read of
/// `ceiling + 1` stored rows is judged in the unit it was asked in.
pub(super) fn book_stored_rows(events: &[vike_marketdata::L2BookUpdate]) -> usize {
    events.iter().map(|e| (e.bids.len() + e.asks.len()).max(1)).sum()
}

/// The named refusal [`range_verb`] answers a no-`limit` request over `ceiling` with: the verb, the
/// series, the range, the ceiling by value AND by name, and the remedy.
fn range_refusal(
    what: &RangeVerb,
    series: &str,
    range: TimestampRange,
    ceiling: usize,
) -> DatahubRefusal {
    DatahubRefusal::RangeOverCeiling {
        verb: what.verb,
        series: series.to_string(),
        range: bounds(range),
        ceiling,
        unit: what.unit,
        ceiling_name: what.ceiling_name,
    }
}

/// Whether `request` is a RANGE read a [`RangeReadGate`] counts: `LoadBars` and the data plane's scans,
/// whose rows (and a JSON reply's frame) sit in this daemon's memory until the reply is written. The
/// strategy lane's three reads (`StrategyFundingHistory`, `StrategyInputBars`, `StrategyForecasts`) are
/// NOT counted: a live mount never waits behind a backtest's page.
pub(super) fn is_range_read(request: &Request) -> bool {
    matches!(
        request,
        Request::LoadBars { .. }
            | Request::ScanQuotes { .. }
            | Request::ScanTrades { .. }
            | Request::ScanBookUpdates { .. }
            | Request::ScanDepth { .. }
            | Request::ScanCohort { .. }
            | Request::ScanPerpMetrics { .. }
            | Request::ScanChain { .. }
            | Request::ScanEquity { .. }
            | Request::ScanTexts { .. }
            | Request::ScanFinancialFacts { .. }
            | Request::ScanInstitutionalHoldings { .. }
            | Request::ScanTextScores { .. }
            | Request::ScanForecasts { .. }
            | Request::ScanExecFills { .. }
            | Request::ScanExecOrders { .. }
    )
}

/// ONE server's count of range reads in flight, across its connections: at most `cap` at once
/// ([`RANGE_READS_IN_FLIGHT`]), and at most `per_client` of them for any one client
/// ([`RANGE_READS_PER_CLIENT`](vike_datahub_client::proto::RANGE_READS_PER_CLIENT)), so no client can
/// hold every place and the others always get one. A client is the key that authenticated the
/// connection, or its peer's address on a key-less server. A [`RangeReadSlot`] is one read's place,
/// released when it drops.
///
/// One per server, made by [`Self::leaked`] when the server starts and carried in its `ConnectionLimits`,
/// so two servers in one process (the test suites spawn dozens) never wait on each other.
#[derive(Debug)]
pub(super) struct RangeReadGate {
    cap: usize,
    per_client: usize,
    held: std::sync::Mutex<RangeReadsHeld>,
    freed: std::sync::Condvar,
}

/// What a [`RangeReadGate`] holds: the places taken, and how many each client holds (a client with none
/// has no entry; there are at most `cap` entries).
#[derive(Debug, Default)]
struct RangeReadsHeld {
    total: usize,
    by_client: Vec<(String, usize)>,
}

impl RangeReadsHeld {
    fn of(&mut self, client: &str) -> Option<&mut usize> {
        self.by_client.iter_mut().find(|(c, _)| c == client).map(|(_, n)| n)
    }
}

impl RangeReadGate {
    /// A gate of `cap` places, `per_client` of them at most for one client, that lives as long as the
    /// process: a server holds one for its whole life, so the leak is one small value per server start.
    pub(super) fn leaked(cap: usize, per_client: usize) -> &'static Self {
        Box::leak(Box::new(Self {
            cap,
            per_client,
            held: std::sync::Mutex::default(),
            freed: std::sync::Condvar::new(),
        }))
    }

    /// A place for one range read of `client`, waiting while the gate is full or `client` holds its
    /// share. Poison or not: nothing panics while the count is locked.
    pub(super) fn take(&'static self, client: &str) -> RangeReadSlot {
        let mut held = self.held.lock().unwrap_or_else(|p| p.into_inner());
        while held.total >= self.cap || held.of(client).is_some_and(|n| *n >= self.per_client) {
            held = self.freed.wait(held).unwrap_or_else(|p| p.into_inner());
        }
        held.total += 1;
        match held.of(client) {
            Some(n) => *n += 1,
            None => held.by_client.push((client.to_string(), 1)),
        }
        RangeReadSlot { gate: self, client: client.to_string() }
    }

    /// [`Self::take`] when `request` is a range read ([`is_range_read`]), else nothing to wait for.
    pub(super) fn admit(&'static self, request: &Request, client: &str) -> Option<RangeReadSlot> {
        is_range_read(request).then(|| self.take(client))
    }
}

/// One range read's place in a [`RangeReadGate`], held by the connection loop until the read's reply is
/// written (an LZ4 reply gives it back before its write: the rows are gone by then) and released on drop,
/// an early return, a write fault and an unwind included.
pub(super) struct RangeReadSlot {
    gate: &'static RangeReadGate,
    client: String,
}

impl Drop for RangeReadSlot {
    fn drop(&mut self) {
        let mut held = self.gate.held.lock().unwrap_or_else(|p| p.into_inner());
        held.total -= 1;
        if let Some(n) = held.of(&self.client) {
            *n -= 1;
        }
        held.by_client.retain(|(_, n)| *n > 0);
        drop(held);
        // Every waiter: one waits for the gate, another for its own share, and either may now go.
        self.gate.freed.notify_all();
    }
}

#[cfg(test)]
#[path = "range_reads_gate_tests.rs"]
mod gate_tests;

/// `LoadBars`' bounded store read, `read(n)` for [`range_verb`] / [`range_rows`]: the venue's candles
/// (`load_bars_head`), or one `source=` lane's (`load_bars_sourced_capped`). The JSON reply, the bar
/// columns and `StrategyInputBars` read through it, so the three read the same rows.
pub(super) fn bars_head<'a>(
    store: &'a (dyn vike_data::HistoryStore + Send + Sync),
    venue: &'a str,
    symbol: &'a str,
    interval: &'a str,
    range: TimestampRange,
    source: Option<&'a str>,
) -> impl FnOnce(usize) -> Result<Vec<vike_marketdata::Bar>, vike_data::HistoryStoreError> + 'a {
    move |n| match source {
        Some(lane) => store.load_bars_sourced_capped(venue, lane, symbol, interval, range, Some(n)),
        None => store.load_bars_head(venue, symbol, interval, range, n),
    }
}

/// The `stored_rows` of every row kind but the book's: one stored row per answered row.
pub(super) fn row_count<T>(rows: &[T]) -> usize {
    rows.len()
}
