//! `vike-cli data universe` — what the store CONTAINED over a window, not what it holds today.
//!
//! # The survivorship-bias defence, and why a plain listing is not one
//!
//! `crate::cmd::data`'s `list` answers "what is in the store". Every backtest-universe bug this
//! verb exists for comes from treating that answer as "what was tradeable then". A perp that was
//! delisted in March, a dated future that expired, a prediction market that resolved, a pair that
//! was renamed — each one leaves a tape that STOPS. Ask the store today which symbols it has and
//! you get the ones that are still running; select a universe that way and backtest a year, and
//! the losers have been removed from the sample by the venue rather than by the strategy. The
//! reported Sharpe is then a measurement of which instruments survived, which is a fact about the
//! venue's listing desk and not about the strategy at all.
//!
//! The defence is the same one LEAN, QuantRocket and Freqtrade each implement at their own seam: a
//! universe that knows its MEMBERSHIP AS OF A DATE, so a run over 2025 selects from the instruments
//! that existed in 2025 — delisted ones included, with their tape ending where it ended.
//!
//! # ⚠ WHAT THE STORE CAN AND CANNOT TELL YOU, and the difference is the whole honesty of this verb
//!
//! This verb reports the FIRST and LAST recorded row of each instrument. That is **evidence of**
//! listing and delisting; it is **not a listing calendar**, and the two are different enough that
//! conflating them would make this verb a liar in exactly the way
//! `super::tape_health`'s own doc is about:
//!
//! * A tape that starts on 2025-03-04 may mean the instrument listed that day, or that a backfill
//!   started there. The store cannot tell those apart, and neither can this verb.
//! * A tape that stops on 2025-09-01 may mean the instrument was delisted, or that the recorder
//!   died, or that nobody has backfilled past that date yet. `data list --gaps` and `data coverage`
//!   are the verbs that discriminate the second and third from the first, and neither is folded in
//!   here — a verb that guessed would be asserting a venue fact it has no source for.
//!
//! So every column below is named for what it MEASURES (`FIRST`/`LAST` recorded, `GONE` days since
//! the store's own newest row) and never for what it implies (`listed`/`delisted`). The STATUS cell
//! is the one derived judgement, and it is derived only from timestamps this side can see. An
//! AUTHORITATIVE calendar — the venue's own listing and delisting dates, an expiry from a dated
//! contract's symbol, a resolution timestamp from a prediction market — is a separate source this
//! workspace does not record anywhere today; see the report accompanying this change.
//!
//! # The reference frame is the STORE's span, not now()
//!
//! "This instrument's tape stopped" is meaningless on its own — every tape has stopped, at the last
//! row anyone wrote. It becomes the survivorship signal only relative to a store that KEPT GOING:
//! an instrument whose last row is 200 days before the newest row in the store is one the store
//! stopped being able to record while it went on recording everything else.
//!
//! That is why the frame is the store's own `[first, last]` over every series it reported — taken
//! BEFORE the filter narrows anything, so a `--venue binance` run and an unfiltered one place the
//! same instrument at the same distance from the edge. Wall-clock `now()` is deliberately not the
//! frame: against a store nobody has backfilled for a month, every instrument in it would read as
//! having left, and the one verb whose job is to find the instruments that stopped would flag all
//! of them.
//!
//! # ⚠ `--from`/`--to` are ACCEPTED here and refused on `list`/`coverage`
//!
//! On the other read verbs a time bound is refused: their answers are whole-series folds of a
//! manifest, so a window could only ever narrow the rendering and not the question. Here the window
//! IS the question — "what was in the universe between these dates" — so the two flags carry their
//! `fetch`/`export` spellings (epoch-ms, or `YYYY-MM-DD`) and are parsed on THIS side by
//! `vike_model::parse_date_label` rather than forwarded. `--days` stays refused: it counts back
//! from NOW, and a membership window anchored to the wall clock answers a different question every
//! time it is run, which is not a thing to compare two backtests against.
//!
//! Both bounds are optional and INDEPENDENT, for `super::ExportRange`'s reason: each of the four
//! combinations is a well-formed question, "the whole of what the store spans" included.

use std::collections::{BTreeMap, BTreeSet};

use vike_model::epoch_ms_to_utc_date;

use super::{col, scope_cell};

/// Epoch-ms per UTC day — the `GONE` column's unit. Declared here for the reason
/// `super::tape_health`'s own copy is: `vike-data`, where this constant already lives twice, is a
/// DEV-dependency of this crate.
const DAY_MS: i64 = 86_400_000;

/// `universe`'s membership window, ALREADY PARSED to epoch-ms.
///
/// ⚠ Parsed at the door rather than forwarded as text, which is the one place this diverges from
/// `super::ExportRange`. That struct carries STRINGS because the ENGINE parses them — one timestamp
/// parser in the workspace, and this side deliberately does not own a second. Nothing is forwarded
/// here: the comparison happens in this process, against numbers a datahub sent, so this side must
/// parse and must therefore refuse a bound it cannot read — as a usage error, before a socket.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct MembershipWindow {
    /// `--from`, or `None` for "as far back as the store goes".
    pub(super) from: Option<i64>,
    /// `--to`, or `None` for "up to the store's newest row".
    pub(super) to: Option<i64>,
}

/// One stored series' span, flattened to plain scalars at the one conversion site in
/// [`super::execute_universe`] — see `super::SeriesRow` for why the read half never keeps the wire
/// types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SeriesSpan {
    pub(super) kind: String,
    pub(super) venue: String,
    /// The symbol for a per-symbol series, the group for a grouped one — `SeriesId::label()`.
    pub(super) name: String,
    pub(super) grouped: bool,
    pub(super) first_ts: i64,
    pub(super) last_ts: i64,
    pub(super) rows: u64,
}

/// One instrument, folded across every kind that carries it.
///
/// ⚠ **Grouped and per-symbol series of the same name are DIFFERENT instruments**, as they are in
/// `vike_data::coverage`'s `InstrumentKey`, and for its stated reason: they are different
/// directories with different manifests, and a family recorded per-symbol before grouping existed
/// genuinely has its history in two places. Merging them here would hide exactly the migration a
/// point-in-time universe is being asked about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Member {
    pub(super) venue: String,
    pub(super) name: String,
    pub(super) grouped: bool,
    /// Which kinds carry this instrument, in first-seen order over a `kind`-sorted fold.
    pub(super) kinds: Vec<String>,
    /// The earliest recorded row across every kind — `None` when every one of this instrument's
    /// series folded to the store's EMPTY coverage, which is a real state and not a missing field.
    /// Rendered as `-`, never as `1970-01-01`: `super::span_cell` carries that argument.
    pub(super) first_ts: Option<i64>,
    /// The latest recorded row across every kind. `None` on the same condition as `first_ts`.
    pub(super) last_ts: Option<i64>,
    pub(super) rows: u64,
}

/// The store's own recorded span — the frame every membership verdict is relative to. See the
/// module doc for why it is this and not `now()`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct StoreSpan {
    pub(super) first_ts: Option<i64>,
    pub(super) last_ts: Option<i64>,
}

/// What one instrument's tape says about its membership of the window.
///
/// Four independent booleans rather than one enum, because `listed_inside` and `left_inside` can
/// BOTH be true — an instrument that appeared in March and stopped in June is the single most
/// interesting row this verb produces, and an enum would have had to invent a fifth variant to say
/// so (or, worse, pick one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Membership {
    /// The recorded span overlaps the window at all.
    pub(super) present: bool,
    /// Recorded from before the window opened until after it closed — the instrument a naive
    /// "symbols the store has" selection would also have picked, i.e. the one that is not a
    /// survivorship problem.
    pub(super) covers_window: bool,
    /// The FIRST recorded row falls strictly inside the window: the instrument appeared mid-run.
    pub(super) listed_inside: bool,
    /// The LAST recorded row falls strictly inside the window: the tape STOPPED while the store
    /// went on. **This is the survivorship signal** — the instrument a listing taken from today's
    /// store would silently omit from a backtest of this window.
    pub(super) left_inside: bool,
    /// UTC days between this instrument's last recorded row and the store's newest — `None` unless
    /// [`Self::left_inside`]. It is the SIZE of the signal: a tape three days behind the edge is a
    /// lagging backfill, one three hundred days behind is an instrument that is gone.
    pub(super) gone_days: Option<i64>,
}

impl Membership {
    /// The verdict for an instrument that cannot be judged: one that recorded nothing at all, or
    /// one in a store that reported no span for there to be a frame.
    ///
    /// A constructor rather than the literal written twice, because the two sites that need it —
    /// [`classify`]'s early return and [`super::execute_universe`]'s no-frame path — must answer
    /// IDENTICALLY. A hand-copied literal that drifted on one field would make the same instrument
    /// read differently depending on whether the STORE was empty or the INSTRUMENT was.
    pub(super) fn absent() -> Self {
        Membership {
            present: false,
            covers_window: false,
            listed_inside: false,
            left_inside: false,
            gone_days: None,
        }
    }

    /// The STATUS cell, and the token a `--json` consumer branches on.
    ///
    /// ⚠ Every one of these names a MEASUREMENT, never a venue fact. `left` is "the tape stopped
    /// inside the window", not "delisted" — the module doc carries why this side may not claim the
    /// second, and a status string is the easiest place in the whole verb to accidentally do it.
    pub(super) fn as_str(self) -> &'static str {
        match (self.present, self.covers_window, self.listed_inside, self.left_inside) {
            (false, _, _, _) => "absent",
            (true, true, _, _) => "whole",
            (true, false, true, true) => "began+left",
            (true, false, true, false) => "began",
            (true, false, false, true) => "left",
            // ⚠ **REACHABLE, and worth naming rather than folding into one of the four above.**
            // The instrument overlaps the window, began before it, and its tape ends before the
            // window closes — yet it is NOT the survivorship signal, because it runs all the way to
            // the store's own newest row (see `left_inside`'s second condition). What this row says
            // is that the WINDOW reaches past what the store has recorded: the operator asked about
            // a stretch nobody has backfilled yet. Calling it `left` would blame the instrument for
            // the store's edge; calling it `whole` would claim coverage that does not exist.
            (true, false, false, false) => "partial",
        }
    }
}

/// Fold per-series spans into per-instrument membership facts.
///
/// PURE, and the min/max ignore ZERO-ROW series deliberately: the store folds an empty series to an
/// all-zero coverage, so admitting one would pull every instrument's `first_ts` back to
/// `1970-01-01` and make the whole report read as though the store began at the epoch. An
/// instrument whose every series is empty keeps `None` for both endpoints and is reported `absent`,
/// which is what it is.
///
/// Output is ordered by `(venue, name, grouped)` so two runs over one store are byte-identical and
/// a report is diffable across a backfill — the same property `super::list_lines` gets for free
/// from the server's own ordering, and which a fold has to establish for itself.
pub(super) fn fold_members(spans: &[SeriesSpan]) -> Vec<Member> {
    let mut by_instrument: BTreeMap<(String, String, bool), (BTreeSet<String>, Member)> =
        BTreeMap::new();
    for s in spans {
        let key = (s.venue.clone(), s.name.clone(), s.grouped);
        let entry = by_instrument.entry(key).or_insert_with(|| {
            (
                BTreeSet::new(),
                Member {
                    venue: s.venue.clone(),
                    name: s.name.clone(),
                    grouped: s.grouped,
                    kinds: Vec::new(),
                    first_ts: None,
                    last_ts: None,
                    rows: 0,
                },
            )
        });
        entry.0.insert(s.kind.clone());
        entry.1.rows = entry.1.rows.saturating_add(s.rows);
        if s.rows == 0 {
            continue;
        }
        entry.1.first_ts = Some(entry.1.first_ts.map_or(s.first_ts, |f| f.min(s.first_ts)));
        entry.1.last_ts = Some(entry.1.last_ts.map_or(s.last_ts, |l| l.max(s.last_ts)));
    }
    by_instrument
        .into_values()
        .map(|(kinds, mut member)| {
            member.kinds = kinds.into_iter().collect();
            member
        })
        .collect()
}

/// The store's own recorded span, over EVERY series the server reported — the frame, taken before
/// any filter narrows it. Zero-row series are skipped for [`fold_members`]' reason.
pub(super) fn store_span(spans: &[SeriesSpan]) -> StoreSpan {
    let mut out = StoreSpan::default();
    for s in spans.iter().filter(|s| s.rows > 0) {
        out.first_ts = Some(out.first_ts.map_or(s.first_ts, |f| f.min(s.first_ts)));
        out.last_ts = Some(out.last_ts.map_or(s.last_ts, |l| l.max(s.last_ts)));
    }
    out
}

/// Resolve the window an operator asked for against the store's own span.
///
/// An unbounded side takes the store's endpoint, which is what makes a bare `data universe` mean
/// "over everything this store covers" rather than an error. When the store reported nothing at all
/// there is no frame and no verdict to give, so this answers `None` and every instrument comes back
/// `absent` — the honest answer, and the one [`lines`] renders with the count of what was reported.
pub(super) fn resolve_window(window: MembershipWindow, store: StoreSpan) -> Option<(i64, i64)> {
    let from = window.from.or(store.first_ts)?;
    let to = window.to.or(store.last_ts)?;
    Some((from, to))
}

/// Classify one instrument against a resolved window and the store's newest row.
///
/// PURE. `from`/`to` are INCLUSIVE, matching `vike_data::TsRange::of`'s own `[start, end]` reading
/// of a range — a membership question asked over `[Jan 1, Dec 31]` must include both days, and an
/// exclusive end would drop the last day of every window an operator types.
pub(super) fn classify(
    member: &Member,
    from: i64,
    to: i64,
    store_last_ts: Option<i64>,
) -> Membership {
    let (Some(first), Some(last)) = (member.first_ts, member.last_ts) else {
        return Membership::absent();
    };
    let present = last >= from && first <= to;
    let covers_window = present && first <= from && last >= to;
    let listed_inside = present && first > from;
    // ⚠ TWO conditions, and the second is the one that makes this a signal rather than a tautology.
    // The tape must stop before the window closes AND before the store's own newest row: a store
    // whose every series ends on the same day has not lost anything, it has simply been read to its
    // edge, and flagging all of it would be the report crying wolf on its own frame.
    let left_inside =
        present && last < to && store_last_ts.is_some_and(|store_last| last < store_last);
    let gone_days = if left_inside {
        store_last_ts.map(|store_last| store_last.saturating_sub(last) / DAY_MS)
    } else {
        None
    };
    Membership { present, covers_window, listed_inside, left_inside, gone_days }
}

/// The human `universe` table — PURE, unit-tested below.
///
/// One row per instrument, then a summary that counts the three answers apart. The three counts are
/// the product: `whole` is what a naive listing would have given you, and the other two are what it
/// would have silently dropped.
pub(super) fn lines(
    rows: &[(Member, Membership)],
    reported: usize,
    narrowed: bool,
    window: Option<(i64, i64)>,
) -> Vec<String> {
    if rows.is_empty() {
        return vec![super::empty_note("instruments", reported, narrowed)];
    }
    let venue_w = col("VENUE", rows.iter().map(|(m, _)| m.venue.len()));
    let scope_w = col("SCOPE", rows.iter().map(|(m, _)| scope_cell(m.grouped).len()));
    let name_w = col("INSTRUMENT", rows.iter().map(|(m, _)| m.name.len()));
    let kinds_w = col("KINDS", rows.iter().map(|(m, _)| m.kinds.join(",").len()));
    let status_w = col("STATUS", rows.iter().map(|(_, s)| s.as_str().len()));

    let mut lines = Vec::new();
    // The window is stated ABOVE the table rather than left implicit. Half of these rows'
    // verdicts are relative to a bound the operator may not have typed (an unbounded side takes
    // the store's own endpoint), so a reader who cannot see the window cannot check a STATUS.
    if let Some((from, to)) = window {
        lines.push(format!(
            "membership over {} .. {} (inclusive)",
            epoch_ms_to_utc_date(from),
            epoch_ms_to_utc_date(to)
        ));
        lines.push(String::new());
    }
    lines.push(format!(
        "{:<venue_w$}  {:<scope_w$}  {:<name_w$}  {:<kinds_w$}  {:<10}  {:<10}  {:<status_w$}  {}",
        "VENUE", "SCOPE", "INSTRUMENT", "KINDS", "FIRST", "LAST", "STATUS", "GONE"
    ));
    for (m, status) in rows {
        lines.push(format!(
            "{:<venue_w$}  {:<scope_w$}  {:<name_w$}  {:<kinds_w$}  {:<10}  {:<10}  \
             {:<status_w$}  {}",
            m.venue,
            scope_cell(m.grouped),
            m.name,
            m.kinds.join(","),
            date_cell(m.first_ts),
            date_cell(m.last_ts),
            status.as_str(),
            gone_cell(status),
        ));
    }
    lines.push(String::new());
    let whole = rows.iter().filter(|(_, s)| s.covers_window).count();
    let began = rows.iter().filter(|(_, s)| s.listed_inside).count();
    let left = rows.iter().filter(|(_, s)| s.left_inside).count();
    let head = if narrowed {
        format!("{} of {reported} instruments", rows.len())
    } else {
        format!("{reported} instruments")
    };
    lines.push(format!(
        "{head} · {whole} spanned the whole window · {began} began inside it · {left} stopped \
         inside it"
    ));
    // ⚠ The survivorship sentence is printed only when there IS one, and it names the DEFENCE
    // rather than the finding. A verb that explained survivorship bias on every run — including the
    // runs where nothing stopped — would be a paragraph an operator scrolls past, and it would be
    // scrolling past it on the run where it mattered too.
    if left > 0 {
        lines.push(format!(
            "⚠ {left} instrument(s) stopped recording inside this window while the store went on. \
             A universe taken from what the store holds TODAY omits them, and a backtest over this \
             window then measures which instruments survived it. Select on this report's rows, not \
             on `data list`."
        ));
    }
    lines
}

/// A recorded endpoint as a UTC date, or `-` when the instrument recorded nothing at all — never a
/// `1970-01-01` synthesized out of an absent value. `super::span_cell` argues the same rule for a
/// `list` row's zero-row series.
fn date_cell(ts: Option<i64>) -> String {
    ts.map_or_else(|| "-".to_string(), epoch_ms_to_utc_date)
}

/// The `GONE` cell: days between this instrument's last row and the store's newest, or `-` when the
/// tape runs to the store's own edge. A number here is the SIZE of the survivorship signal.
fn gone_cell(status: &Membership) -> String {
    status.gone_days.map_or_else(|| "-".to_string(), |d| format!("{d}d"))
}

/// One instrument as a `--json` object.
///
/// It carries the measured timestamps beside the derived verdict for `super::tape_health`'s reason:
/// a consumer that disagrees with a STATUS must be able to re-derive it. `status` is the string,
/// and the four booleans are carried too rather than folded into it — `began+left` is two facts,
/// and a consumer filtering for "everything that stopped" should not have to know which composite
/// strings contain the word.
pub(super) fn json_members(members: &[(Member, Membership)]) -> Vec<serde_json::Value> {
    members
        .iter()
        .map(|(m, s)| {
            serde_json::json!({
                "venue": m.venue,
                "name": m.name,
                "grouped": m.grouped,
                "kinds": m.kinds,
                "rows": m.rows,
                "first_recorded_ts": m.first_ts,
                "last_recorded_ts": m.last_ts,
                "first_recorded_date": m.first_ts.map(epoch_ms_to_utc_date),
                "last_recorded_date": m.last_ts.map(epoch_ms_to_utc_date),
                "status": s.as_str(),
                "present": s.present,
                "covers_window": s.covers_window,
                "began_inside": s.listed_inside,
                "stopped_inside": s.left_inside,
                "gone_days": s.gone_days,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-01-01T00:00:00Z — an exact UTC midnight, so every date in these tests reads as the day
    /// its offset names.
    const JAN1: i64 = 1_767_225_600_000;

    fn span(kind: &str, name: &str, first_day: i64, last_day: i64) -> SeriesSpan {
        SeriesSpan {
            kind: kind.to_string(),
            venue: "binance".to_string(),
            name: name.to_string(),
            grouped: false,
            first_ts: JAN1 + first_day * DAY_MS,
            last_ts: JAN1 + last_day * DAY_MS,
            rows: 100,
        }
    }

    fn day(n: i64) -> i64 {
        JAN1 + n * DAY_MS
    }

    #[test]
    fn the_fold_joins_kinds_and_takes_the_widest_span() {
        let members = fold_members(&[
            span("trade", "BTCUSDT", 0, 10),
            span("bar", "BTCUSDT", 3, 20),
            span("quote", "BTCUSDT", 5, 8),
        ]);
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].kinds, vec!["bar", "quote", "trade"], "kind-sorted, deterministic");
        assert_eq!(members[0].first_ts, Some(day(0)), "the earliest of any kind");
        assert_eq!(members[0].last_ts, Some(day(20)), "the latest of any kind");
        assert_eq!(members[0].rows, 300);
    }

    /// ⚠ A zero-row series must not pull an instrument's start back to the epoch. The store folds
    /// an empty series to an all-zero coverage, so admitting one would make every affected
    /// instrument read as having begun on 1970-01-01.
    #[test]
    fn an_empty_series_does_not_drag_the_span_to_the_epoch() {
        let mut empty = span("quote", "BTCUSDT", 0, 0);
        empty.first_ts = 0;
        empty.last_ts = 0;
        empty.rows = 0;
        let members = fold_members(&[span("bar", "BTCUSDT", 5, 9), empty]);
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].first_ts, Some(day(5)));
        assert_eq!(members[0].kinds, vec!["bar", "quote"], "the empty kind is still NAMED");
    }

    /// An instrument whose every series is empty keeps no endpoints and is `absent` — not a member
    /// with a 1970 span, and not silently dropped either.
    #[test]
    fn an_instrument_with_only_empty_series_is_absent() {
        let mut empty = span("bar", "GHOSTUSDT", 0, 0);
        empty.first_ts = 0;
        empty.last_ts = 0;
        empty.rows = 0;
        let members = fold_members(&[empty]);
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].first_ts, None);
        let status = classify(&members[0], day(0), day(30), Some(day(30)));
        assert!(!status.present);
        assert_eq!(status.as_str(), "absent");
    }

    #[test]
    fn grouped_and_per_symbol_instruments_do_not_merge() {
        let mut grouped = span("trade", "fam", 0, 5);
        grouped.grouped = true;
        let members = fold_members(&[span("trade", "fam", 0, 5), grouped]);
        assert_eq!(members.len(), 2, "{members:#?}");
    }

    /// An instrument recorded across the whole window is the one a naive listing also finds — the
    /// baseline this verb measures the other answers against.
    #[test]
    fn a_tape_spanning_the_window_is_whole() {
        let members = fold_members(&[span("bar", "BTCUSDT", 0, 100)]);
        let status = classify(&members[0], day(10), day(20), Some(day(100)));
        assert!(status.covers_window);
        assert_eq!(status.as_str(), "whole");
        assert_eq!(status.gone_days, None);
    }

    /// **The survivorship case this verb exists for.** The tape stops on day 20 while the store
    /// records to day 100: a universe taken from the store today omits this instrument entirely,
    /// and a backtest over days 0..60 then samples only what survived.
    #[test]
    fn a_tape_that_stops_while_the_store_goes_on_is_the_survivorship_signal() {
        let members = fold_members(&[span("bar", "DEADUSDT", 0, 20)]);
        let status = classify(&members[0], day(0), day(60), Some(day(100)));
        assert!(status.present, "it WAS in the universe for part of the window");
        assert!(status.left_inside);
        assert!(!status.covers_window);
        assert_eq!(status.as_str(), "left");
        assert_eq!(status.gone_days, Some(80), "the size of the signal, in days behind the edge");
    }

    /// ⚠ **The tautology this check has to avoid.** Every tape stops somewhere, so "stopped before
    /// the window's end" alone would flag an instrument that simply runs to the store's own edge —
    /// i.e. every instrument in a store whose window reaches past its newest row.
    #[test]
    fn a_tape_running_to_the_stores_own_edge_never_reads_as_having_left() {
        let members = fold_members(&[span("bar", "BTCUSDT", 0, 100)]);
        // The window extends beyond anything the store holds; the tape still reaches the edge.
        let status = classify(&members[0], day(0), day(365), Some(day(100)));
        assert!(!status.left_inside, "{status:?}");
        assert_eq!(status.gone_days, None);
        // …and it lands on the fifth verdict rather than being squeezed into one of the four:
        // the WINDOW outran the store, which is not the same fact as an instrument stopping.
        assert_eq!(status.as_str(), "partial", "{status:?}");
    }

    #[test]
    fn a_tape_beginning_inside_the_window_is_reported_as_such() {
        let members = fold_members(&[span("bar", "NEWUSDT", 30, 100)]);
        let status = classify(&members[0], day(0), day(60), Some(day(100)));
        assert!(status.listed_inside);
        assert!(!status.left_inside);
        assert_eq!(status.as_str(), "began");
    }

    /// Both at once — the row that an enum would have had to pick one half of.
    #[test]
    fn a_tape_that_began_and_stopped_inside_the_window_says_both() {
        let members = fold_members(&[span("bar", "BRIEFUSDT", 10, 20)]);
        let status = classify(&members[0], day(0), day(60), Some(day(100)));
        assert!(status.listed_inside && status.left_inside);
        assert_eq!(status.as_str(), "began+left");
    }

    /// A tape entirely outside the window is absent from it, even though the store holds it — which
    /// is the whole point of asking the question as of a date.
    #[test]
    fn a_tape_outside_the_window_is_absent_from_it() {
        let members = fold_members(&[span("bar", "OLDUSDT", 0, 5)]);
        let status = classify(&members[0], day(50), day(60), Some(day(100)));
        assert!(!status.present);
        assert_eq!(status.as_str(), "absent");
    }

    /// The bounds are INCLUSIVE at both ends: a tape covering exactly the window's two endpoints
    /// spans it, and an exclusive end would have dropped the last day of every window typed.
    #[test]
    fn the_window_bounds_are_inclusive() {
        let members = fold_members(&[span("bar", "BTCUSDT", 10, 20)]);
        let status = classify(&members[0], day(10), day(20), Some(day(20)));
        assert!(status.covers_window, "{status:?}");
        assert_eq!(status.as_str(), "whole");
    }

    // ── the frame and the window resolution ─────────────────────────────────────────────────────

    #[test]
    fn the_frame_is_the_stores_own_span_over_every_reported_series() {
        let store = store_span(&[span("bar", "A", 5, 40), span("trade", "B", 0, 100)]);
        assert_eq!(store.first_ts, Some(day(0)));
        assert_eq!(store.last_ts, Some(day(100)));
    }

    #[test]
    fn an_unbounded_window_takes_the_stores_endpoints() {
        let store = StoreSpan { first_ts: Some(day(0)), last_ts: Some(day(100)) };
        assert_eq!(
            resolve_window(MembershipWindow::default(), store),
            Some((day(0), day(100))),
            "a bare `data universe` asks about everything the store spans"
        );
        assert_eq!(
            resolve_window(MembershipWindow { from: Some(day(7)), to: None }, store),
            Some((day(7), day(100))),
            "one bound alone is a well-formed question; the other takes the store's"
        );
    }

    /// A store that reported nothing gives no frame, so there is no verdict to render — answered
    /// as `None` rather than as a window over zero.
    #[test]
    fn an_empty_store_yields_no_window_at_all() {
        assert_eq!(resolve_window(MembershipWindow::default(), StoreSpan::default()), None);
        // …and an operator's own bounds do not manufacture one either: the frame's far side is
        // still missing, and inventing `now()` for it is exactly what the module doc refuses.
        let asked = MembershipWindow { from: Some(day(0)), to: None };
        assert_eq!(resolve_window(asked, StoreSpan::default()), None);
    }

    // ── the renderings ──────────────────────────────────────────────────────────────────────────

    fn classified(spans: &[SeriesSpan], from: i64, to: i64) -> Vec<(Member, Membership)> {
        let store = store_span(spans);
        fold_members(spans)
            .into_iter()
            .map(|m| {
                let status = classify(&m, from, to, store.last_ts);
                (m, status)
            })
            .collect()
    }

    /// The table states the WINDOW above itself: half these verdicts are relative to a bound the
    /// operator may never have typed, so a reader who cannot see it cannot check a STATUS.
    #[test]
    fn the_rendering_states_the_window_it_judged_against() {
        let rows = classified(&[span("bar", "BTCUSDT", 0, 100)], day(0), day(100));
        let out = lines(&rows, 1, false, Some((day(0), day(100))));
        assert!(out[0].contains("membership over"), "{}", out[0]);
        assert!(out[0].contains("2026-01-01"), "{}", out[0]);
        assert!(out[0].contains("inclusive"), "{}", out[0]);
    }

    /// The summary counts the three answers apart, and the survivorship warning fires ONLY when
    /// something actually stopped — a paragraph printed every run is a paragraph nobody reads.
    #[test]
    fn the_survivorship_warning_fires_only_when_something_stopped() {
        let healthy = classified(&[span("bar", "BTCUSDT", 0, 100)], day(0), day(100));
        let out = lines(&healthy, 1, false, Some((day(0), day(100))));
        let text = out.join("\n");
        assert!(text.contains("1 spanned the whole window"), "{text}");
        assert!(text.contains("0 stopped inside it"), "{text}");
        assert!(!text.contains("survived"), "no warning when nothing stopped: {text}");

        let mixed = classified(
            &[span("bar", "BTCUSDT", 0, 100), span("bar", "DEADUSDT", 0, 20)],
            day(0),
            day(100),
        );
        let out = lines(&mixed, 2, false, Some((day(0), day(100))));
        let text = out.join("\n");
        assert!(text.contains("1 stopped inside it"), "{text}");
        assert!(text.contains("survived"), "the warning names the defence: {text}");
        assert!(text.contains("data list"), "…and what NOT to select on: {text}");
    }

    /// An instrument that recorded nothing renders `-` for both endpoints rather than a
    /// synthesized 1970 date.
    #[test]
    fn an_absent_instrument_renders_dashes_not_the_epoch() {
        let mut empty = span("bar", "GHOSTUSDT", 0, 0);
        empty.first_ts = 0;
        empty.last_ts = 0;
        empty.rows = 0;
        let rows = classified(&[empty], day(0), day(100));
        let out = lines(&rows, 1, false, None);
        let text = out.join("\n");
        assert!(!text.contains("1970"), "{text}");
        assert!(text.contains("absent"), "{text}");
    }

    #[test]
    fn nothing_matched_is_reported_as_a_filter_outcome() {
        let out = lines(&[], 9, true, None);
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("9 reported"), "{}", out[0]);
    }

    /// The document carries the measured timestamps AND the four booleans beside the status string,
    /// so a consumer filtering for "everything that stopped" never has to substring-match a
    /// composite verdict.
    ///
    /// ⚠ The long-running BTCUSDT span is not decoration — it is what makes the FRAME extend past
    /// BRIEFUSDT's last row. With the short tape alone the store's newest row IS that tape's last
    /// row, [`classify`]'s second condition holds, and the verdict is `began` rather than
    /// `began+left`. That is the tautology guard doing its job, and a fixture that omitted the
    /// second span would have been asserting the wrong thing about the right code.
    #[test]
    fn the_document_carries_the_booleans_beside_the_status() {
        let spans = [span("bar", "BRIEFUSDT", 10, 20), span("bar", "BTCUSDT", 0, 100)];
        let rows = classified(&spans, day(0), day(60));
        let doc = json_members(&rows);
        assert_eq!(doc.len(), 2, "ordered by (venue, name): BRIEFUSDT sorts before BTCUSDT");
        assert_eq!(doc[0]["name"], serde_json::json!("BRIEFUSDT"));
        assert_eq!(doc[0]["status"], serde_json::json!("began+left"));
        assert_eq!(doc[0]["stopped_inside"], serde_json::json!(true));
        assert_eq!(doc[0]["began_inside"], serde_json::json!(true));
        assert_eq!(doc[0]["last_recorded_date"], serde_json::json!("2026-01-21"));
        assert_eq!(doc[0]["gone_days"], serde_json::json!(80));
    }
}
