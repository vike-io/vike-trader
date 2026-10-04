//! The RANGE-read verb family — `LoadBars` and the seven scans (`ScanQuotes`, `ScanTrades`,
//! `ScanBookUpdates`, `ScanDepth`, `ScanCohort`, `ScanPerpMetrics`, `ScanEquity`) through
//! [`range_verb`], and `ScanExecFills` through [`exec_fills_verb`]: a store read that stops at what
//! one reply can carry, a whole-`ts` row cap, and an exact byte fit to the frame. `handle_request`
//! (the parent module) holds the arms and passes each its [`RangeVerb`] vocabulary and ceiling; the
//! ceilings themselves (`LOAD_BARS_CEILING` and its siblings, [`ReadCeilings`]) are public constants
//! of the parent module and stay there.

use super::*;

/// Cap `rows` to about `limit`, cutting ONLY on a whole-`ts` boundary.
///
/// ⚠ **The softness is the point.** A paging client continues from `last_ts + 1` — the reply
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
/// one; `vike_data::store_kind`'s codecs sort on `ts` first for every kind.
fn cap_to_whole_ts<T>(mut rows: Vec<T>, limit: Option<u32>, ts_of: impl Fn(&T) -> i64) -> Vec<T> {
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
pub(super) const SCAN_EQUITY: RangeVerb =
    RangeVerb { verb: "ScanEquity", unit: "equity samples", ceiling_name: "SCAN_EQUITY_CEILING" };

/// One range verb — `LoadBars` and the seven scans — answered from a read that stops at what the
/// reply can carry. `read(n)` is the verb's bounded store read for `n` rows: a COMPLETE PREFIX of
/// the range holding at least `n` stored rows unless it is the whole range (`HistStore`'s
/// `load_bars_head`, or a `scan_*_capped` with `Some(n)`). `stored_rows` counts what the store's `n`
/// counts — rows, except for the book kinds, whose store counts one row per LEVEL
/// ([`book_stored_rows`]).
///
/// ⚠ **The store is asked for a HEAD, never for the range.** Every one of these arms used to read
/// the client's whole range — `LoadBars` through `load_bars`, the three research scans through their
/// unbudgeted reads on EVERY page, the four tick scans whenever no `limit` was sent — and then
/// [`cap_to_whole_ts`], so the frame was bounded and the allocation was whatever the client asked
/// for: gigabytes for one request over a long `5s` series, and, through `RemoteHistStore`'s pager,
/// the rest of the range again for every page. Both arms below need only the head:
///
/// * **With a `limit` `L`**, the store is asked for `min(L, ceiling)` rows and the reply is
///   [`cap_to_whole_ts`] of them — BYTE-IDENTICAL to the old reply whenever `L` is within the
///   ceiling, because a complete prefix of at least `L` rows holds every row the cap's decision
///   looks at (`docs/superpowers/specs/2026-10-01-loadbars-bounded-read-design.md`'s section 2
///   carries the three cases). A `limit` above the ceiling is CLAMPED to it: a page shorter than
///   its `limit` is already legal under the soft cap, and every reply longer than the ceiling
///   overran the frame anyway (see [`LOAD_BARS_CEILING`] and its siblings).
/// * **With no `limit`** — and with `Some(0)`, which [`cap_to_whole_ts`] has always read as "no
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
#[allow(clippy::too_many_arguments)] // the verb, its series, range, limit, two ceilings, three row functions, its variant
pub(super) fn range_verb<T: serde::Serialize>(
    what: &RangeVerb,
    series: &str,
    range: TsRange,
    limit: Option<u32>,
    ceiling: usize,
    frame_bytes: usize,
    read: impl FnOnce(usize) -> Result<Vec<T>, vike_data::DataError>,
    stored_rows: impl Fn(&[T]) -> usize,
    ts_of: impl Fn(&T) -> i64,
    wrap: fn(Vec<T>) -> Response,
) -> Response {
    match limit.filter(|l| *l > 0) {
        Some(limit) => {
            let n = (limit as usize).min(ceiling);
            let rows = match read(n) {
                Ok(rows) => rows,
                Err(e) => return Response::Error(e.to_string()),
            };
            // `n <= limit`, so it is a `u32` again without loss.
            let rows = cap_to_whole_ts(rows, Some(n as u32), &ts_of);
            match fit_to_frame(rows, wrap, frame_bytes, &ts_of) {
                FrameFit::Whole(rows) | FrameFit::Prefix(rows) => wrap(rows),
                FrameFit::FirstTsOver { ts } => {
                    Response::Error(first_ts_over_frame(what, series, ts, frame_bytes))
                }
            }
        }
        None => {
            let rows = match read(ceiling.saturating_add(1)) {
                Ok(rows) => rows,
                Err(e) => return Response::Error(e.to_string()),
            };
            if stored_rows(&rows) > ceiling {
                return Response::Error(range_refusal(what, series, range, ceiling));
            }
            match fit_to_frame(rows, wrap, frame_bytes, &ts_of) {
                FrameFit::Whole(rows) => wrap(rows),
                FrameFit::Prefix(_) | FrameFit::FirstTsOver { .. } => {
                    Response::Error(range_over_frame(what, series, range, frame_bytes))
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
    FirstTsOver { ts: i64 },
}

/// Cut `rows` (ts-ascending) to the longest whole-`ts` prefix whose reply — `wrap`'s variant
/// around them — fits `frame_bytes`, counting EXACTLY rather than estimating: each row's JSON is
/// written into a [`ByteCount`] that allocates nothing, plus one separating comma per row after the
/// first, plus the envelope (the length of `wrap` around no rows, measured once). That is the body
/// `write_frame` builds, byte for byte, so "fits here" and "passes `write_frame`" are the same
/// statement. The cost is one extra serialisation pass of CPU and no memory.
///
/// ⚠ **It NEVER answers an empty prefix in place of rows.** A paging client stops on its first
/// EMPTY page (`RemoteHistStore`'s `paged`), so an empty answer from a range that goes on reads as
/// its end and the rest is lost with no error — the loss
/// `docs/superpowers/specs/2026-10-01-tick-scan-paging-silent-loss-design.md` fixed, made again by a
/// different cap. When the first timestamp's rows alone overrun the frame the answer is
/// [`FrameFit::FirstTsOver`], which every caller turns into a NAMED error.
///
/// No row width is assumed anywhere, so there is no "skip the count when every row is narrow"
/// shortcut: even a bar carries an `Option<String>` symbol, so no kind here has a widest row.
fn fit_to_frame<T: serde::Serialize>(
    mut rows: Vec<T>,
    wrap: fn(Vec<T>) -> Response,
    frame_bytes: usize,
    ts_of: impl Fn(&T) -> i64,
) -> FrameFit<T> {
    let mut total = json_len(&wrap(Vec::new()));
    // `rows[..boundary]` is the longest whole-`ts` prefix known to fit: it is moved only at a `ts`
    // change, and only while every row before it has been counted without passing the frame.
    let mut boundary = 0usize;
    for i in 0..rows.len() {
        if i > 0 && ts_of(&rows[i]) != ts_of(&rows[i - 1]) {
            boundary = i;
        }
        total = total.saturating_add(usize::from(i > 0)).saturating_add(json_len(&rows[i]));
        if total > frame_bytes {
            if boundary == 0 {
                return FrameFit::FirstTsOver { ts: ts_of(&rows[0]) };
            }
            rows.truncate(boundary);
            return FrameFit::Prefix(rows);
        }
    }
    FrameFit::Whole(rows)
}

/// An `io::Write` that keeps only a COUNT of what is written to it — so [`json_len`] measures a
/// row's exact JSON without building it.
struct ByteCount(usize);

impl io::Write for ByteCount {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The exact length of `value`'s compact JSON — what `serde_json::to_vec` would produce, and so
/// what it contributes to a frame body. A value serde cannot write (none of the row types here
/// can fail) counts as larger than any frame, which refuses rather than sends it.
fn json_len<T: serde::Serialize + ?Sized>(value: &T) -> usize {
    let mut count = ByteCount(0);
    match serde_json::to_writer(&mut count, value) {
        Ok(()) => count.0,
        Err(_) => usize::MAX,
    }
}

/// The named refusal for a reply with no `limit` whose rows number no more than their ceiling but
/// whose JSON passes the frame — the case a row ceiling derived from the SHORTEST row cannot see.
fn range_over_frame(what: &RangeVerb, series: &str, range: TsRange, frame_bytes: usize) -> String {
    let bound = |b: Option<i64>| b.map_or_else(|| "open".to_string(), |t| t.to_string());
    format!(
        "{} {series} over [{}, {}] is more than one reply can carry: its {} pass MAX_FRAME_LEN \
         ({frame_bytes} bytes) of JSON although they number no more than {}, and this request \
         sent no `limit`, so it is refused rather than sent to fail at the frame. Send a `limit` \
         and start each next request at the last `ts` + 1 of the reply before it \
         (`RemoteHistStore` pages that way), or narrow the range.",
        what.verb,
        bound(range.start),
        bound(range.end),
        what.unit,
        what.ceiling_name,
    )
}

/// The named error for a page whose FIRST timestamp's rows alone pass the frame — answered in place
/// of an EMPTY page, which a pager would read as the end of the range.
fn first_ts_over_frame(what: &RangeVerb, series: &str, ts: i64, frame_bytes: usize) -> String {
    format!(
        "{} {series}: the {} at ts {ts} alone pass MAX_FRAME_LEN ({frame_bytes} bytes), so no \
         page can carry them; none is sent, because an empty page would read as the end of the \
         range. This range cannot be read past ts {ts} over this wire.",
        what.verb, what.unit,
    )
}

/// The rows a book answer stands for in the STORE's count — one per level, and one placeholder for
/// an event with none (`vike_data`'s book codec, `book_rows`) — so a no-`limit` read of
/// `ceiling + 1` stored rows is judged in the unit it was asked in.
pub(super) fn book_stored_rows(events: &[vike_model::BookUpdate]) -> usize {
    events.iter().map(|e| (e.bids.len() + e.asks.len()).max(1)).sum()
}

/// The named refusal [`range_verb`] answers a no-`limit` request over `ceiling` with: the verb, the
/// series, the range, the ceiling by value AND by name, and the remedy.
fn range_refusal(what: &RangeVerb, series: &str, range: TsRange, ceiling: usize) -> String {
    let bound = |b: Option<i64>| b.map_or_else(|| "open".to_string(), |t| t.to_string());
    format!(
        "{} {series} over [{}, {}] holds more than {ceiling} {} ({}, the most one reply can \
         carry) and this request sent no `limit`, so it is refused and the rest of the range was \
         not read. Send a `limit` and start each next request at the last `ts` + 1 of the reply \
         before it (`RemoteHistStore` pages that way), or narrow the range.",
        what.verb,
        bound(range.start),
        bound(range.end),
        what.unit,
        what.ceiling_name,
    )
}

/// `Request::ScanExecFills`, answered from a HEAD of `ceiling + 1` fills: the whole series when it
/// holds no more than `ceiling`, and a named refusal when it holds more. The variant carries no range
/// and no `limit`, so this is [`range_verb`]'s no-`limit` arm and nothing else — see
/// [`SCAN_EXEC_FILLS_CEILING`] for why a refusal here takes nothing away. Its byte check is
/// [`range_verb`]'s no-`limit` one: a series under the ceiling whose fills' JSON passes the frame is
/// refused by name too.
pub(super) fn exec_fills_verb(
    store: &Arc<dyn HistStore + Send + Sync>,
    venue: &str,
    symbol: &str,
    ceiling: usize,
    frame_bytes: usize,
) -> Response {
    let rows = match store.scan_exec_fills_head(
        venue,
        symbol,
        TsRange::all(),
        ceiling.saturating_add(1),
    ) {
        Ok(rows) => rows,
        Err(e) => return Response::Error(e.to_string()),
    };
    if rows.len() > ceiling {
        return Response::Error(format!(
            "ScanExecFills {venue}:{symbol} holds more than {ceiling} fills \
             (SCAN_EXEC_FILLS_CEILING, the most one reply can carry), and this verb carries no \
             range and no `limit` to page with, so it is refused and the rest of the series was not \
             read. A range and a limit on this verb is a wire change deferred until a series nears \
             this ceiling."
        ));
    }
    match fit_to_frame(rows, Response::ExecFills, frame_bytes, |r| r.ts) {
        FrameFit::Whole(rows) => Response::ExecFills(rows),
        FrameFit::Prefix(_) | FrameFit::FirstTsOver { .. } => Response::Error(format!(
            "ScanExecFills {venue}:{symbol} is more than one reply can carry: its fills' JSON \
             passes MAX_FRAME_LEN ({frame_bytes} bytes) although they number no more than \
             SCAN_EXEC_FILLS_CEILING, and this verb carries no range and no `limit` to page with, \
             so it is refused rather than sent to fail at the frame."
        )),
    }
}

/// The `stored_rows` of every row kind but the book's: one stored row per answered row.
pub(super) fn row_count<T>(rows: &[T]) -> usize {
    rows.len()
}
