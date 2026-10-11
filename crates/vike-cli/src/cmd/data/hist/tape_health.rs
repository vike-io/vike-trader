//! `vike-cli data hist health` — the series whose OWN CATALOG contradicts itself.
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
//! FILE: they are other repositories, and `crates/vike-ops/tests/docs/citation_gate/rules.rs`'s
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
//!    the SERVER's or the ENGINE's work, and `vike_data::store::quality` — which already folds scanned
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
//! `vike_data::store::coverage`'s `partial_days` gives for narrowing to an instrument's own recorded
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
// `crates/vike-model/src/orders/order.rs`'s `MS_PER_DAY` is the constant, and
// `crates/vike-model/src/lib.rs` re-exports it at the crate ROOT — one `use` away, in a crate this
// file already imports twice. The OTHER half is true and is beside the point — `vike-data`,
// where `crates/vike-data/src/store/coverage.rs` and `crates/vike-data/src/store/quality.rs` each declare
// their own copy, is a DEV-dependency here (the module doc's type wall), so neither of those can
// be imported; the value never had to come from there. `crate::cmd::data::gate` inherited the
// false sentence from this one and was corrected with it.
use vike_model::time::interval_ms;
use vike_model::{MS_PER_DAY, time::epoch_ms_to_utc_date};
use vike_node_proto::auth::{NodeKeys, Scope};

use super::{Args, col, scope_cell, tape_health};
use crate::cmd::data::shared::connect;
use crate::exit::CmdResult;

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
pub(crate) struct Scanned {
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

/// The ROW-LEVEL checks `tape-health` does NOT run, named in its own `--json` document.
///
/// ⚠ **Declared as data, not as a comment, and that is the whole point.** A health report whose
/// clean verdict could be mistaken for a COMPLETE one is worse than no report: an operator reads
/// "nothing contradicts itself" and concludes the OHLC is sane, which this verb never looked at.
/// So the document carries what was skipped, by name, and `crate::cmd::data::tape_health`'s module
/// doc carries the two walls behind it — the `vike_data::TsRange` parameter on every row-reading
/// RPC (this crate takes `vike-data` as a DEV-dependency, so that type cannot be named in library
/// code), and the compute-to-data rule that says a fold over a year of bars belongs beside the
/// Parquet rather than across a socket.
///
/// The human rendering states the same bound in `--help` rather than under every run, for the
/// reason `tape_health::lines` gives for the survivorship sentence one verb over: a paragraph
/// printed unconditionally is a paragraph that is skipped on the run where it mattered.
const ROW_CHECKS_NOT_RUN: [&str; 3] =
    ["ohlc-bounds", "row-duplicate-timestamp", "row-non-monotonic"];

/// `data hist tape-health` — one `inventory()` round trip, then a pure fold per matched series.
///
/// ⚠ **No row crosses the wire, and that is a property rather than an optimization.** This is the
/// same single RPC [`execute_list`] makes; every finding below is arithmetic over the coverage
/// numbers the server already sent. There is deliberately no per-series second round trip the way
/// `gaps` makes one: a gap probe answers a question the catalog does not already contain, and
/// every check here is answerable from what one `inventory()` carries.
pub(super) fn execute_tape_health(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    let mut client = connect(&args.addr, keys, Scope::Read)?;
    let inventory = client.inventory()?;
    let reported = inventory.len();

    let mut scanned: Vec<tape_health::Scanned> = Vec::new();
    for (id, cov) in &inventory {
        if !args.filter.matches(Some(&id.kind), &id.venue, id.label()) {
            continue;
        }
        let facts = tape_health::SeriesFacts {
            kind: id.kind.clone(),
            venue: id.venue.clone(),
            name: id.label().to_string(),
            grouped: id.group.is_some(),
            interval: id.interval.clone(),
            first_ts: cov.first_ts,
            last_ts: cov.last_ts,
            rows: cov.rows,
            parts: cov.parts,
            dates: cov.dates,
        };
        let findings = tape_health::findings_for(&facts);
        scanned.push(tape_health::Scanned { facts, findings });
    }

    if args.json {
        println!("{}", tape_health_json(args, &scanned, reported));
    } else {
        for line in tape_health::lines(&scanned, reported, !args.filter.is_empty()) {
            println!("{line}");
        }
    }
    Ok(())
}

/// The `tape-health --json` document.
///
/// Every scanned series appears, the clean ones included — unlike the human rendering, which shows
/// only the offenders. The two are deliberately different: a person is reading a verdict and wants
/// the exceptions, while a machine reader is folding several stores and needs to know a series was
/// LOOKED AT and passed. An absent row would otherwise be indistinguishable from a filter that
/// never selected it.
pub(crate) fn tape_health_json(
    args: &Args,
    scanned: &[tape_health::Scanned],
    reported: usize,
) -> String {
    let (contradictions, suspects) = tape_health::totals(scanned);
    let series = tape_health::json_series(scanned);
    let doc = serde_json::json!({
        // ⚠ DERIVED from the verb, never re-typed. These four renderers each carried the name as a
        // LITERAL, so the group split renamed `list` -> `ls` and `tape-health` -> `health` in the
        // CLI while the JSON document kept saying the old word — a document naming a spelling the
        // parser refuses. Four copies of one fact, found by the rename rather than by reading.
        "subcommand": args.sub.as_str(),
        "addr": args.addr,
        "filter": {
            "kind": args.filter.kind,
            "venue": args.filter.venue,
            "name": args.filter.name,
        },
        "series_reported": reported,
        "count": series.len(),
        "with_findings": scanned.iter().filter(|s| !s.clean()).count(),
        // The two classes are carried APART and never summed — see `tape_health::Class`.
        "contradictions": contradictions,
        "suspects": suspects,
        // What this verb did NOT check. See [`ROW_CHECKS_NOT_RUN`].
        "row_checks_not_run": ROW_CHECKS_NOT_RUN,
        "series": series,
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, numbers, bools and nulls; serialization is total")
}

// ─── `universe`: point-in-time membership ───────────────────────────────────────────────────────

#[path = "tape_health_tests.rs"]
#[cfg(test)]
mod tape_health_tests;
