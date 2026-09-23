//! `vike-cli data tape-health` — the series whose OWN CATALOG contradicts itself.
//!
//! # Absence is visible; FALSEHOOD is not
//!
//! Every other read verb in [`crate::cmd::data`] reports what is MISSING. `list --gaps` names the
//! holes inside a recorded span; `coverage` names the day one kind has and another lacks. An
//! operator who runs either one and sees nothing has learned something true: the tape is there.
//!
//! What neither can see is a tape that is PRESENT and self-contradictory — two bars stamped the
//! same millisecond, a span that ends before it begins, more `date=` partitions than the span has
//! days. A backtest over data like that does not fail. It runs, it reports a Sharpe, and the number
//! is a lie: a duplicated bar is folded twice, so the strategy trades twice on one event and the
//! equity curve is the sum of a decision taken once and settled twice. That is strictly worse than
//! a gap, because a gap shows up as a flat stretch in the equity curve and a duplicate shows up as
//! alpha.
//!
//! So this verb asks the opposite question from its siblings: not *what is missing*, but *is what
//! is here even possible*. Prior art asks it at the same seam — Qlib's data-health check (missing
//! and duplicate index entries, plus an OHLC sanity pass), RQAlpha's bundle validation, and
//! GoCryptoTrader's `datahistory` candle validation, which refuses a fetched candle whose `low`
//! exceeds its `open`/`close` before it ever reaches a store. ⚠ None of the three is cited by
//! FILE: they are other repositories, and `crates/vike-ops/tests/citation_gate.rs`'s
//! `every_fully_qualified_path_citation_resolves` existence-checks any backticked path anchored at
//! one of this tree's own top-level directories — a foreign `scripts/…` reddens it.
//!
//! # ⚠ It is NOT a row scan, and the name says `tape-health` rather than `tape-health-scan` for
//! # exactly that reason
//!
//! Three of the checks this verb would ideally run need the ROWS: `low <= open,close <= high`,
//! per-row duplicate timestamps, and per-row monotonicity. **None of them is reachable from this
//! crate, and that is a property of the architecture rather than an oversight.** Two independent
//! walls, and the second is the one that matters:
//!
//! 1. **The type wall.** `vike_datahub_client::DatahubClient`'s `load_bars` / `scan_quotes` /
//!    `scan_trades` each take a `vike_data::TsRange`, and `crates/vike-cli/Cargo.toml` declares
//!    `vike-data` a DEV-dependency — so that parameter cannot be NAMED in this crate's library
//!    code at all. It is the same wall `crate::cmd::data`'s module doc already records for
//!    `vike_data::SeriesId`, which is why the read verbs flatten the wire types on arrival and why
//!    `list --gaps` hands a `SeriesId` straight back rather than constructing one.
//! 2. **The compute-to-data rule.** `vike_datahub_client::client`'s module doc states the design
//!    outright: the thin client "never pulls a raw UPSTREAM data slice across the wire — a read
//!    verb returns exactly the query's bounded result". Checking `low <= high` over a year of
//!    minute bars means shipping half a million rows to a process whose entire identity is being
//!    DataFusion-free, in order to run a fold that belongs beside the Parquet. A row-level scan is
//!    the SERVER's or the ENGINE's work, and `vike_data::quality` — which already folds scanned
//!    rows into a per-day trust record — is where its neighbour already lives.
//!
//! So the ROW half is declared, not faked. What this verb does instead is the half the CATALOG
//! already proves, in ONE `inventory()` round trip with no row on the wire, and several of those
//! proofs are the very checks the brief names: a bar series whose row count exceeds the number of
//! grid slots its own span holds **is** a duplicate-timestamp finding, arrived at by pigeonhole
//! rather than by reading a timestamp.
//!
//! # A PROOF and a SMELL are different findings, and collapsing them is how a report gets ignored
//!
//! [`Class::Contradiction`] is arithmetic: the numbers the store reported cannot all be true of any
//! series that exists. [`Class::Suspect`] is a shape only ever seen from a defect but not
//! impossible on its face. They are reported as separate counts and never summed, for the reason
//! `vike_data::coverage`'s `partial_days` gives for narrowing to an instrument's own recorded
//! kinds: a warning that fires on something legitimate is a warning an operator learns to skim
//! past, and it takes the real ones with it.
//!
//! # Only the series with findings are rendered
//!
//! The default output is the offenders and a count of what was scanned — not one row per series.
//! `crate::cmd::data`'s `list` is the listing; this is a verdict, and a verdict that prints a clean
//! line for every healthy series buries the one line that matters. There is deliberately no
//! `--all` flag to widen it: the summary already states how many series were scanned, so a
//! narrower-than-expected scan is visible without one.

// ⚠ The day constant is IMPORTED, and this is a CORRECTION worth keeping. This file used to
// declare `const DAY_MS: i64 = 86_400_000;` above a doc reading "and `vike-model` — which this
// crate does link — exports no day constant". That half was FALSE the day it was written:
// `crates/vike-model/src/order.rs`'s `MS_PER_DAY` is the constant, and
// `crates/vike-model/src/lib.rs` re-exports it at the crate ROOT — one `use` away, in a crate this
// file already imports twice. The OTHER half is true and is beside the point — `vike-data`,
// where `crates/vike-data/src/coverage.rs` and `crates/vike-data/src/quality.rs` each declare
// their own copy, is a DEV-dependency here (the module doc's type wall), so neither of those can
// be imported; the value never had to come from there. `crate::cmd::data::gate` inherited the
// false sentence from this one and was corrected with it.
use vike_model::time::interval_ms;
use vike_model::{MS_PER_DAY, epoch_ms_to_utc_date};

use super::{col, scope_cell};

/// How strong a finding is — and the two are counted separately, never summed. See the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Class {
    /// The reported numbers cannot all be true of any series that exists. Arithmetic, not opinion.
    Contradiction,
    /// Possible on its face, but only ever produced by a defect — a half-written commit, a resample
    /// anchored to the wrong origin. Worth looking at; not proof on its own.
    Suspect,
}

impl Class {
    /// The token both renderings use, and the string a `--json` consumer branches on.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Class::Contradiction => "contradiction",
            Class::Suspect => "suspect",
        }
    }
}

/// One thing wrong with one series.
///
/// `code` is a STABLE machine token and `detail` carries the numbers that prove it. Two fields
/// rather than one sentence because a wrapper that wants to alert on `rows-exceed-grid` must not
/// have to match on prose — and because the numbers are the whole evidence, so a code without them
/// would be an assertion an operator cannot check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Finding {
    pub(super) code: &'static str,
    pub(super) class: Class,
    pub(super) detail: String,
}

/// The per-series numbers a finding can be derived from — the whole of `vike_data::SeriesCoverage`
/// plus the identity, flattened to plain scalars at the one conversion site in
/// [`super::execute_tape_health`].
///
/// Flattened for the reason `super::SeriesRow` is: the wire types name a DEV-dependency, so this
/// side reads their public fields and keeps none of their shapes. It also makes every check below a
/// pure function over plain data, which is what lets them be unit-tested against planted
/// impossibilities rather than against a store somebody has to build first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SeriesFacts {
    pub(super) kind: String,
    pub(super) venue: String,
    /// The symbol for a per-symbol series, the group for a grouped one — `SeriesId::label()`.
    pub(super) name: String,
    pub(super) grouped: bool,
    /// `Some` for bars, `None` for every tick-shaped kind. The grid checks below run only when it
    /// is `Some` AND parses, because a bar step is the only thing that makes a grid exist.
    pub(super) interval: Option<String>,
    pub(super) first_ts: i64,
    pub(super) last_ts: i64,
    pub(super) rows: u64,
    pub(super) parts: usize,
    pub(super) dates: usize,
}

/// One series and what is wrong with it. A `Scanned` with an empty `findings` is a series that
/// passed, and is deliberately kept: the counts in the summary are computed from the same vector
/// the offenders are rendered from, so "how many were scanned" and "how many were reported" cannot
/// disagree about one series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Scanned {
    pub(super) facts: SeriesFacts,
    pub(super) findings: Vec<Finding>,
}

impl Scanned {
    /// `true` when nothing was found — the ordinary case, and the one that is not rendered.
    pub(super) fn clean(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Every structural contradiction the catalog alone proves about one series.
///
/// PURE, and each check states the impossibility it rests on. The order is fixed (worst-shaped
/// first) so two runs over one store produce byte-identical output, which is what makes this
/// diffable across a backfill.
///
/// ⚠ **A series the store folded to EMPTY produces nothing.** An all-zero coverage with zero rows
/// is the store's own representation of "this series has no rows" — `super::span_cell` renders it
/// as `-` for exactly that reason — so treating it as a `1970-01-01` span would manufacture a
/// finding on every empty series in the store. Emptiness is absence, which is `list`'s question.
pub(super) fn findings_for(facts: &SeriesFacts) -> Vec<Finding> {
    let mut out = Vec::new();
    let empty_fold = facts.rows == 0 && facts.first_ts == 0 && facts.last_ts == 0;
    if empty_fold {
        // Nothing here is a contradiction: it is the store saying "no rows", which is absence.
        // `parts` is still worth a word, and the check below is reached on this path deliberately.
        push_parts_without_rows(facts, &mut out);
        return out;
    }

    // ── the span itself ─────────────────────────────────────────────────────────────────────────
    if facts.rows > 0 && facts.last_ts < facts.first_ts {
        out.push(Finding {
            code: "span-inverted",
            class: Class::Contradiction,
            detail: format!(
                "last_ts {} is BEFORE first_ts {} ({} .. {}) — a series cannot end before it \
                 begins, so at least one of the two endpoints is not the endpoint of these {} rows",
                facts.last_ts,
                facts.first_ts,
                epoch_ms_to_utc_date(facts.last_ts),
                epoch_ms_to_utc_date(facts.first_ts),
                facts.rows,
            ),
        });
    }
    if facts.rows > 0 && facts.first_ts == 0 && facts.last_ts == 0 {
        out.push(Finding {
            code: "rows-without-span",
            class: Class::Contradiction,
            detail: format!(
                "{} rows with an all-zero span. An all-zero coverage is the store's own fold for a \
                 series with NO rows, so these two facts describe different series",
                facts.rows
            ),
        });
    }
    if facts.rows == 0 {
        out.push(Finding {
            code: "span-without-rows",
            class: Class::Contradiction,
            detail: format!(
                "no rows, but a span of {} .. {} ({}..{}). The store folds an empty series to an \
                 all-zero coverage, so a NON-zero span here was measured over rows the index can \
                 no longer see",
                facts.first_ts,
                facts.last_ts,
                epoch_ms_to_utc_date(facts.first_ts),
                epoch_ms_to_utc_date(facts.last_ts),
            ),
        });
    }
    push_parts_without_rows(facts, &mut out);
    // ── the on-disk shape against the span, then the bar grid ───────────────────────────────────
    push_partition_finding(facts, &mut out);
    push_grid_findings(facts, &mut out);
    out
}

/// More `date=` partitions than the recorded span has UTC days.
///
/// `dates` counts partition directories; the span bounds how many UTC days the rows inside them can
/// fall in. More partitions than days is not a judgement about cadence — it is a count that cannot
/// be, so the class is [`Class::Contradiction`].
///
/// Skipped on an INVERTED span, deliberately: the day count is then negative and every series would
/// report this, adding a second finding that says nothing the first did not, computed from numbers
/// already known to be wrong.
fn push_partition_finding(facts: &SeriesFacts, out: &mut Vec<Finding>) {
    if facts.rows == 0 || facts.last_ts < facts.first_ts {
        return;
    }
    let (Ok(dates), Some(days)) = (i64::try_from(facts.dates), span_days(facts)) else {
        return;
    };
    if dates <= days {
        return;
    }
    out.push(Finding {
        code: "days-exceed-span",
        class: Class::Contradiction,
        detail: format!(
            "{dates} `date=` partitions over a span of {days} UTC day(s) ({} .. {}). Every row \
             lies inside the span, so the partitions holding them cannot outnumber the days it \
             covers",
            epoch_ms_to_utc_date(facts.first_ts),
            epoch_ms_to_utc_date(facts.last_ts),
        ),
    });
}

/// Part files on disk that contribute no rows at all.
///
/// SUSPECT rather than a contradiction, and the distinction is real: a Parquet part holding zero
/// rows is a well-formed file, so this is possible without anything being broken. What produces it
/// in practice is a commit that wrote its parts and did not land its manifest rows — which is
/// exactly the state a reader sees as an empty series sitting on top of data it will never read.
fn push_parts_without_rows(facts: &SeriesFacts, out: &mut Vec<Finding>) {
    if facts.parts > 0 && facts.rows == 0 {
        out.push(Finding {
            code: "parts-without-rows",
            class: Class::Suspect,
            detail: format!(
                "{} part file(s) on disk and 0 rows in the index — a commit that wrote its parts \
                 without landing their rows leaves exactly this, and nothing will ever read them",
                facts.parts
            ),
        });
    }
}

/// The two findings that need a BAR GRID, and therefore a parseable interval.
///
/// Split out of [`findings_for`] so that function stays inside one screen and so the guard that
/// makes both checks meaningful — a step that parses to a positive number of ms — is written once.
/// A tick-shaped kind (`trade`/`quote`/`book`/`depth`) carries no interval and is skipped entirely:
/// a trade tape has no grid to be off, and a second trade in the same millisecond is ordinary.
fn push_grid_findings(facts: &SeriesFacts, out: &mut Vec<Finding>) {
    let Some(ivl) = facts.interval.as_deref() else { return };
    // ⚠ An UNPARSEABLE interval is treated as NO GRID, not as a finding of its own. Which
    // spellings exist is `vike_model::time::interval_ms`' business and a store may hold a kind
    // this CLI has never heard of — the same rule `super::check_spec` follows for a venue name.
    let Some(step) = interval_ms(ivl).filter(|s| *s > 0) else { return };
    if facts.rows == 0 || facts.last_ts < facts.first_ts {
        return;
    }

    // ⚠ THE DUPLICATE-TIMESTAMP PROOF, and it needs no row.
    //
    // Distinct bars on a `step`-spaced tape are at least `step` apart, so the most a span can hold
    // is `floor((last - first) / step) + 1`. A row count above that is the pigeonhole: two rows lie
    // closer together than one interval, which is either the same timestamp twice or a bar from a
    // finer tape folded into this one. Both make a bar-aligned reader fold one event twice.
    //
    // `saturating_sub` rather than a plain `-`: the guard above already proved the span is not
    // inverted, so the difference is non-negative, and saturating at `i64::MAX` on a nonsense span
    // yields a slot count no row count can exceed — i.e. it declines to report rather than
    // overflowing.
    let reach = facts.last_ts.saturating_sub(facts.first_ts);
    let slots = (reach / step).unsigned_abs() + 1;
    if facts.rows > slots {
        out.push(Finding {
            code: "rows-exceed-grid",
            class: Class::Contradiction,
            detail: format!(
                "{} rows over a span that holds at most {slots} `{ivl}` bars ({} .. {}). Distinct \
                 bars are at least one interval apart, so at least {} row(s) sit closer than \
                 that — a duplicated timestamp, or a finer tape folded in. Either way a \
                 bar-aligned reader folds one event twice",
                facts.rows,
                epoch_ms_to_utc_date(facts.first_ts),
                epoch_ms_to_utc_date(facts.last_ts),
                facts.rows - slots,
            ),
        });
    }

    // ⚠ BOUNDED TO STEPS THAT DIVIDE A UTC DAY, deliberately. A bar's timestamp is its OPEN, which
    // every venue this tree fetches from stamps on a grid anchored at UTC midnight — so for `1m`,
    // `1h` or `12h` an endpoint off that grid is a real finding. For a step that does NOT divide a
    // day (`7h`, `3d`) the epoch-anchored grid this arithmetic would test against is an invention
    // of ours and no venue owes it anything, so the check is skipped rather than made to fire on
    // legitimate data. That is the same rule `Class::Suspect` exists to keep: a check that cannot
    // be trusted takes the trustworthy ones down with it.
    if MS_PER_DAY % step == 0 {
        for (which, ts) in [("first_ts", facts.first_ts), ("last_ts", facts.last_ts)] {
            if ts.rem_euclid(step) != 0 {
                out.push(Finding {
                    code: "endpoint-off-grid",
                    class: Class::Suspect,
                    detail: format!(
                        "{which} {ts} ({}) is {} ms past a `{ivl}` boundary. A bar's timestamp is \
                         its OPEN, stamped on the UTC-midnight grid — an offset one is a resample \
                         anchored to its first row instead of to the grid, and its bars will not \
                         line up with the venue's own",
                        epoch_ms_to_utc_date(ts),
                        ts.rem_euclid(step),
                    ),
                });
            }
        }
    }
}

/// UTC days the span covers, inclusive of both endpoints — `None` when the subtraction would
/// overflow, which is a shape no store produces and which this side declines to guess about.
fn span_days(facts: &SeriesFacts) -> Option<i64> {
    let first_day = facts.first_ts.div_euclid(MS_PER_DAY);
    let last_day = facts.last_ts.div_euclid(MS_PER_DAY);
    last_day.checked_sub(first_day)?.checked_add(1)
}

/// The human `tape-health` rendering — PURE, so every rule here is unit-tested rather than only
/// seen.
///
/// One header row per OFFENDING series, its findings indented under it, then a summary. A clean
/// series is not rendered at all (see the module doc), and the summary is what makes that safe: it
/// states how many were scanned, so an operator can tell "nothing is wrong" from "nothing was
/// looked at".
pub(super) fn lines(scanned: &[Scanned], reported: usize, filtered: bool) -> Vec<String> {
    if scanned.is_empty() {
        return vec![super::empty_note("series", reported, filtered)];
    }
    let offenders: Vec<&Scanned> = scanned.iter().filter(|s| !s.clean()).collect();
    let contradictions = count_of(scanned, Class::Contradiction);
    let suspects = count_of(scanned, Class::Suspect);

    let mut lines = Vec::new();
    if !offenders.is_empty() {
        let kind_w = col("KIND", offenders.iter().map(|s| s.facts.kind.len()));
        let venue_w = col("VENUE", offenders.iter().map(|s| s.facts.venue.len()));
        let scope_w = col("SCOPE", offenders.iter().map(|s| scope_cell(s.facts.grouped).len()));
        let name_w = col("NAME", offenders.iter().map(|s| s.facts.name.len()));
        let ivl_w = col("INTERVAL", offenders.iter().map(|s| grid_cell(&s.facts).len()));
        lines.push(format!(
            "{:<kind_w$}  {:<venue_w$}  {:<scope_w$}  {:<name_w$}  {:<ivl_w$}  {}",
            "KIND", "VENUE", "SCOPE", "NAME", "INTERVAL", "FINDINGS"
        ));
        for s in &offenders {
            lines.push(format!(
                "{:<kind_w$}  {:<venue_w$}  {:<scope_w$}  {:<name_w$}  {:<ivl_w$}  {}",
                s.facts.kind,
                s.facts.venue,
                scope_cell(s.facts.grouped),
                s.facts.name,
                grid_cell(&s.facts),
                s.findings.len(),
            ));
            for f in &s.findings {
                lines.push(format!("      [{}] {}: {}", f.class.as_str(), f.code, f.detail));
            }
        }
        lines.push(String::new());
    }

    let head = if filtered {
        format!("{} of {reported} series scanned", scanned.len())
    } else {
        format!("{} series scanned", scanned.len())
    };
    // ⚠ The clean case is SAID rather than left as an absence of rows. An operator who typed this
    // verb asked a question, and a silent exit is indistinguishable from a verb that did nothing —
    // the same argument `super::gap_lines` makes for printing "no gaps".
    if offenders.is_empty() {
        lines.push(format!("{head} · nothing contradicts itself"));
    } else {
        lines.push(format!(
            "{head} · {} with findings · {contradictions} contradiction(s), {suspects} suspect(s)",
            offenders.len()
        ));
    }
    lines
}

/// How many findings of one class across every scanned series. Counted from the same vector the
/// rows render from, so a summary cannot disagree with the detail above it.
fn count_of(scanned: &[Scanned], class: Class) -> usize {
    scanned.iter().flat_map(|s| s.findings.iter()).filter(|f| f.class == class).count()
}

/// The INTERVAL cell: the bar step, or `-` for a tick-shaped kind that has none — the same rule
/// `super::interval_cell` applies to a `list` row, spelled here because that one takes a
/// `super::SeriesRow` and this table's rows are [`SeriesFacts`].
fn grid_cell(facts: &SeriesFacts) -> String {
    facts.interval.clone().unwrap_or_else(|| "-".to_string())
}

/// One series as a `--json` object — the identity, the numbers the findings were derived FROM, and
/// the findings.
///
/// ⚠ The coverage numbers are carried even for a clean series, and carried UNTOUCHED. A consumer
/// that disagrees with a verdict must be able to re-derive it from the same inputs; a document that
/// shipped only the verdict would make this side's arithmetic unauditable, which is the one thing a
/// data-health report cannot afford to be.
pub(super) fn json_series(scanned: &[Scanned]) -> Vec<serde_json::Value> {
    scanned
        .iter()
        .map(|s| {
            serde_json::json!({
                "kind": s.facts.kind,
                "venue": s.facts.venue,
                "name": s.facts.name,
                "grouped": s.facts.grouped,
                "interval": s.facts.interval,
                "coverage": {
                    "first_ts": s.facts.first_ts,
                    "last_ts": s.facts.last_ts,
                    "rows": s.facts.rows,
                    "parts": s.facts.parts,
                    "dates": s.facts.dates,
                },
                "healthy": s.clean(),
                "findings": s.findings.iter().map(|f| serde_json::json!({
                    "code": f.code,
                    "class": f.class.as_str(),
                    "detail": f.detail,
                })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

/// The two class counts, for the `--json` envelope. Returned as a pair rather than computed twice
/// at the call site for the reason [`count_of`] exists: one fold, one answer.
pub(super) fn totals(scanned: &[Scanned]) -> (usize, usize) {
    (count_of(scanned, Class::Contradiction), count_of(scanned, Class::Suspect))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bar series the store could actually hold: hourly bars, on the grid, one row per slot.
    fn healthy_bars() -> SeriesFacts {
        SeriesFacts {
            kind: "bar".to_string(),
            venue: "binance".to_string(),
            name: "BTCUSDT".to_string(),
            grouped: false,
            interval: Some("1h".to_string()),
            // 2026-01-01T00:00:00Z .. +9h, ten hourly slots, ten rows.
            first_ts: 1_767_225_600_000,
            last_ts: 1_767_225_600_000 + 9 * 3_600_000,
            rows: 10,
            parts: 1,
            dates: 1,
        }
    }

    fn codes(facts: &SeriesFacts) -> Vec<&'static str> {
        findings_for(facts).into_iter().map(|f| f.code).collect()
    }

    #[test]
    fn a_well_formed_bar_series_produces_nothing() {
        assert_eq!(codes(&healthy_bars()), Vec::<&str>::new());
    }

    /// **The check this module exists for.** Eleven rows cannot fit in ten hourly slots, so at
    /// least one timestamp is duplicated (or a finer bar was folded in) — proven by pigeonhole,
    /// with no row on the wire. A backtest over this tape trades twice on one event.
    #[test]
    fn more_rows_than_the_grid_holds_is_a_duplicate_timestamp_proof() {
        let mut facts = healthy_bars();
        facts.rows = 11;
        let found = findings_for(&facts);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].code, "rows-exceed-grid");
        assert_eq!(found[0].class, Class::Contradiction);
        // The evidence is IN the message: both counts and the excess, so an operator can check the
        // arithmetic rather than take the verdict on trust.
        assert!(found[0].detail.contains("11 rows"), "{}", found[0].detail);
        assert!(found[0].detail.contains("at most 10"), "{}", found[0].detail);
    }

    /// Exactly filling the grid is not a finding — the boundary is `>`, not `>=`, because a tape
    /// with one bar per slot is the healthy case and firing on it would make the verb useless.
    #[test]
    fn exactly_filling_the_grid_is_clean() {
        let mut facts = healthy_bars();
        facts.rows = 10;
        assert_eq!(codes(&facts), Vec::<&str>::new());
        // …and FEWER rows than slots is ABSENCE, which `list --gaps` answers. Not this verb's
        // question, and reporting it here would duplicate a report that is already correct.
        facts.rows = 4;
        assert_eq!(codes(&facts), Vec::<&str>::new());
    }

    #[test]
    fn an_inverted_span_is_a_contradiction() {
        let mut facts = healthy_bars();
        std::mem::swap(&mut facts.first_ts, &mut facts.last_ts);
        let found = findings_for(&facts);
        assert!(found.iter().any(|f| f.code == "span-inverted"), "{found:#?}");
        assert!(
            found.iter().all(|f| f.class == Class::Contradiction),
            "an impossible span is arithmetic, never a smell: {found:#?}"
        );
    }

    /// ⚠ The grid checks are SKIPPED on an inverted span rather than run on it. `last - first` is
    /// negative there, so `slots` would be zero or negative and every row count would "exceed the
    /// grid" — a second finding that says nothing the first did not, on numbers that are already
    /// known to be wrong.
    #[test]
    fn an_inverted_span_does_not_also_manufacture_a_grid_finding() {
        let mut facts = healthy_bars();
        std::mem::swap(&mut facts.first_ts, &mut facts.last_ts);
        assert!(!codes(&facts).contains(&"rows-exceed-grid"), "{:#?}", findings_for(&facts));
    }

    /// The store's own empty fold — zero rows, all-zero coverage — is ABSENCE and produces nothing.
    /// Treating it as a 1970 span would put a finding on every empty series in the store, which is
    /// how a report earns its way into an operator's ignore list.
    #[test]
    fn the_stores_empty_fold_is_not_a_finding() {
        let facts = SeriesFacts {
            kind: "trade".to_string(),
            venue: "polymarket".to_string(),
            name: "TOK".to_string(),
            grouped: false,
            interval: None,
            first_ts: 0,
            last_ts: 0,
            rows: 0,
            parts: 0,
            dates: 0,
        };
        assert_eq!(codes(&facts), Vec::<&str>::new());
    }

    /// …but zero rows with a REAL span is the same fold contradicting itself, and it is reported.
    #[test]
    fn zero_rows_with_a_span_is_a_contradiction() {
        let mut facts = healthy_bars();
        facts.rows = 0;
        assert!(codes(&facts).contains(&"span-without-rows"), "{:#?}", findings_for(&facts));
    }

    /// Part files with no rows behind them: possible on its face, so SUSPECT — and it is still
    /// reported on the empty-fold path, because that is the state it actually appears in.
    #[test]
    fn parts_without_rows_is_suspect_and_survives_the_empty_fold() {
        let facts = SeriesFacts {
            kind: "bar".to_string(),
            venue: "demo".to_string(),
            name: "DEMOUSDT".to_string(),
            grouped: false,
            interval: Some("1h".to_string()),
            first_ts: 0,
            last_ts: 0,
            rows: 0,
            parts: 3,
            dates: 0,
        };
        let found = findings_for(&facts);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].code, "parts-without-rows");
        assert_eq!(found[0].class, Class::Suspect);
    }

    #[test]
    fn more_partitions_than_the_span_has_days_is_a_contradiction() {
        let mut facts = healthy_bars();
        // Ten hours inside one UTC day cannot be spread across four `date=` directories.
        facts.dates = 4;
        let found = findings_for(&facts);
        assert!(found.iter().any(|f| f.code == "days-exceed-span"), "{found:#?}");
    }

    /// An endpoint off the UTC-midnight grid is SUSPECT — a resample anchored to its first row
    /// rather than to the grid, whose bars will not line up with the venue's.
    #[test]
    fn an_off_grid_endpoint_is_suspect_when_the_step_divides_a_day() {
        let mut facts = healthy_bars();
        facts.first_ts += 137;
        facts.last_ts += 137;
        let found = findings_for(&facts);
        let off: Vec<&Finding> = found.iter().filter(|f| f.code == "endpoint-off-grid").collect();
        assert_eq!(off.len(), 2, "both endpoints are off the grid: {found:#?}");
        assert!(off.iter().all(|f| f.class == Class::Suspect), "{off:#?}");
        assert!(off[0].detail.contains("137"), "the offset is the evidence: {}", off[0].detail);
    }

    /// ⚠ And it is SKIPPED for a step that does not divide a UTC day. `7h` has no venue-owed grid
    /// this arithmetic could test against, so the check would fire on legitimate data — which is
    /// exactly the failure mode `Class::Suspect` is separated out to avoid.
    #[test]
    fn a_step_that_does_not_divide_a_day_is_not_grid_checked() {
        let mut facts = healthy_bars();
        facts.interval = Some("7h".to_string());
        facts.first_ts += 137;
        facts.last_ts += 137;
        assert!(!codes(&facts).contains(&"endpoint-off-grid"), "{:#?}", findings_for(&facts));
    }

    /// A tick-shaped kind carries no interval, so it has no grid: a second trade in the same
    /// millisecond is ordinary market data, not a defect. Both grid checks are skipped, while the
    /// span and partition checks still apply.
    #[test]
    fn a_tick_series_is_never_grid_checked_but_is_still_span_checked() {
        let mut facts = healthy_bars();
        facts.kind = "trade".to_string();
        facts.interval = None;
        facts.rows = 5_000_000;
        assert_eq!(codes(&facts), Vec::<&str>::new(), "no grid to exceed");

        facts.dates = 99;
        assert!(codes(&facts).contains(&"days-exceed-span"), "{:#?}", findings_for(&facts));
    }

    /// An unparseable interval is treated as NO grid rather than as a finding. Which interval
    /// spellings exist is `vike_model::time::interval_ms`' business and a store may hold a kind
    /// this CLI has never heard of — the same rule `super::check_spec` follows for a venue.
    #[test]
    fn an_unparseable_interval_is_not_itself_a_finding() {
        let mut facts = healthy_bars();
        facts.interval = Some("3q".to_string());
        facts.rows = 10_000;
        assert_eq!(codes(&facts), Vec::<&str>::new());
    }

    // ── the renderings ──────────────────────────────────────────────────────────────────────────

    fn scan(facts: SeriesFacts) -> Scanned {
        let findings = findings_for(&facts);
        Scanned { facts, findings }
    }

    /// A clean scan SAYS SO, and says how many series it looked at — a silent exit could not be
    /// told apart from a filter that matched nothing.
    #[test]
    fn a_clean_scan_states_the_count_it_covered() {
        let out = lines(&[scan(healthy_bars())], 1, false);
        assert_eq!(out.len(), 1, "{out:#?}");
        assert!(out[0].contains("1 series scanned"), "{}", out[0]);
        assert!(out[0].contains("nothing contradicts itself"), "{}", out[0]);
    }

    /// Only the offenders get rows, and the summary carries the two class counts SEPARATELY — a
    /// single total would let one suspect read as one contradiction.
    #[test]
    fn only_offenders_are_rendered_and_the_classes_are_counted_apart() {
        let mut bad = healthy_bars();
        bad.name = "ETHUSDT".to_string();
        bad.rows = 11;
        bad.parts = 0;
        let mut smelly = healthy_bars();
        smelly.name = "SOLUSDT".to_string();
        smelly.first_ts += 137;
        smelly.last_ts += 137;

        let out = lines(&[scan(healthy_bars()), scan(bad), scan(smelly)], 3, false);
        let text = out.join("\n");
        assert!(!text.contains("BTCUSDT"), "the clean series is not rendered: {text}");
        assert!(text.contains("ETHUSDT") && text.contains("SOLUSDT"), "{text}");
        let summary = out.last().expect("a summary line is always last");
        assert!(summary.contains("3 series scanned"), "{summary}");
        assert!(summary.contains("2 with findings"), "{summary}");
        assert!(summary.contains("1 contradiction(s)"), "{summary}");
        assert!(summary.contains("2 suspect(s)"), "both endpoints: {summary}");
    }

    /// An empty scan is the FILTER's answer, not a health verdict — the two have different causes
    /// and `super::empty_note` is the one place that distinction is drawn.
    #[test]
    fn nothing_matched_is_reported_as_a_filter_outcome() {
        let out = lines(&[], 12, true);
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("12 reported"), "{}", out[0]);
    }

    /// The document carries the inputs beside the verdict, for a clean series too, so a consumer
    /// can re-derive the arithmetic instead of trusting it.
    #[test]
    fn the_document_carries_the_numbers_a_verdict_was_derived_from() {
        let doc = json_series(&[scan(healthy_bars())]);
        assert_eq!(doc.len(), 1);
        assert_eq!(doc[0]["healthy"], serde_json::json!(true));
        assert_eq!(doc[0]["coverage"]["rows"], serde_json::json!(10));
        assert_eq!(doc[0]["findings"].as_array().map(|f| f.len()), Some(0));
    }

    #[test]
    fn the_totals_agree_with_the_rendered_summary() {
        let mut bad = healthy_bars();
        bad.rows = 11;
        bad.parts = 0;
        assert_eq!(totals(&[scan(healthy_bars()), scan(bad)]), (1, 0));
    }
}
