//! `coverage`: per instrument, the days one recorded kind has and another lacks.
use vike_model::time::epoch_ms_to_utc_date;
use vike_node_proto::auth::{NodeKeys, Scope};

use super::{Args, InstrumentRow, KindRow, PartialRow};
use crate::cmd::data::shared::{col, connect, empty_note, kinds_cell, scope_cell};
use crate::exit::CmdResult;

/// How many partial DAYS `coverage` renders under one instrument before it stops and says how many
/// it withheld. A half-failed recording can be partial on every day it has, and a report that
/// scrolled a year of dates off the top of a terminal would bury the row it belongs to.
///
/// ⚠ It bounds the HUMAN rendering only. [`coverage_json`] carries every partial day, because a
/// machine reader asked for the report in order to fold it and a truncated array would be a wrong
/// answer rather than a long one.
pub(super) const MAX_PARTIAL_DAYS_SHOWN: usize = 10;

/// `data hist coverage` — one `coverage_report()` round trip, filtered and optionally narrowed to the
/// instruments that actually have a disagreement.
///
/// ⚠ The verb is CAPABILITY-NEGOTIATED on the client side: against a datahub too old to advertise
/// the coverage feature, `coverage_report()` refuses before sending anything and its own sentence
/// says so. That refusal stays on the run-failure rung, not the connect one — the box answered the
/// handshake, so it is a fact about a REACHABLE server and retrying it forever is the inversion the
/// ladder exists to prevent (`crate::cmd::trade::status`'s `failure_exit` argues the same rule
/// against a tradehub node).
pub(super) fn execute_coverage(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    let mut client = connect(&args.addr, keys, Scope::Read)?;
    let report = client.coverage_report()?;
    let reported = report.len();

    let mut rows: Vec<InstrumentRow> = Vec::new();
    for instrument in &report {
        // `None` for the kind dimension: a coverage row IS the join across kinds, which is also why
        // `parse` refuses `--kind` here.
        if !args.filter.matches(None, &instrument.key.venue, &instrument.key.label) {
            continue;
        }
        let partial: Vec<PartialRow> = instrument
            .partial_days()
            .into_iter()
            .map(|p| PartialRow { day: p.day, start_ms: p.start_ms(), missing: p.missing_kinds })
            .collect();
        if args.partial_only && partial.is_empty() {
            continue;
        }
        let kinds = instrument
            .recorded_kinds()
            .into_iter()
            .map(|k| KindRow {
                kind: k.to_string(),
                days: instrument.kinds.get(k).map_or(0, |d| d.present.len()),
            })
            .collect();
        rows.push(InstrumentRow {
            venue: instrument.key.venue.clone(),
            name: instrument.key.label.clone(),
            grouped: instrument.key.grouped,
            kinds,
            spanned_days: instrument.spanned_days().len(),
            partial,
        });
    }

    if args.json {
        println!("{}", coverage_json(args, &rows, reported));
    } else {
        let narrowed = args.partial_only || !args.filter.is_empty();
        for line in coverage_lines(&rows, reported, narrowed) {
            println!("{line}");
        }
    }
    Ok(())
}

/// The human `coverage` table — PURE, unit-tested below.
///
/// One row per instrument, then the partial days indented under it, capped at
/// [`MAX_PARTIAL_DAYS_SHOWN`] with a line saying how many were withheld. A COMPLETE instrument gets
/// its row and no detail lines: there is nothing to explain, and a "no partial days" line under
/// every healthy instrument is how a report teaches an operator to skim past it.
///
/// ⚠ No gap ranges appear here in any form. This table's days are UTC-day INDICES and a series' gap
/// ranges are epoch-ms; both would render as dates and neither would say which it was.
/// `vike-cli data hist gaps` is the per-series view.
pub(crate) fn coverage_lines(
    rows: &[InstrumentRow],
    reported: usize,
    narrowed: bool,
) -> Vec<String> {
    if rows.is_empty() {
        return vec![empty_note("instruments", reported, narrowed)];
    }
    let venue_w = col("VENUE", rows.iter().map(|r| r.venue.len()));
    let scope_w = col("SCOPE", rows.iter().map(|r| scope_cell(r.grouped).len()));
    let name_w = col("INSTRUMENT", rows.iter().map(|r| r.name.len()));
    let kinds_w = col("KINDS", rows.iter().map(|r| kinds_cell(r).len()));
    let days_w = col("DAYS", rows.iter().map(|r| r.spanned_days.to_string().len()));

    let mut lines = vec![format!(
        "{:<venue_w$}  {:<scope_w$}  {:<name_w$}  {:<kinds_w$}  {:>days_w$}  {}",
        "VENUE", "SCOPE", "INSTRUMENT", "KINDS", "DAYS", "PARTIAL"
    )];
    for r in rows {
        let partial_cell =
            if r.partial.is_empty() { "-".to_string() } else { r.partial.len().to_string() };
        lines.push(format!(
            "{:<venue_w$}  {:<scope_w$}  {:<name_w$}  {:<kinds_w$}  {:>days_w$}  {}",
            r.venue,
            scope_cell(r.grouped),
            r.name,
            kinds_cell(r),
            r.spanned_days,
            partial_cell,
        ));
        for p in r.partial.iter().take(MAX_PARTIAL_DAYS_SHOWN) {
            lines.push(format!(
                "      {}  missing: {}",
                epoch_ms_to_utc_date(p.start_ms),
                p.missing.join(", ")
            ));
        }
        if let Some(hidden) = r.partial.len().checked_sub(MAX_PARTIAL_DAYS_SHOWN).filter(|n| *n > 0)
        {
            lines.push(format!("      … and {hidden} more partial days (--json carries them all)"));
        }
    }
    lines.push(String::new());
    let partial_rows = rows.iter().filter(|r| !r.partial.is_empty()).count();
    let head = if narrowed {
        format!("{} of {reported} instruments", rows.len())
    } else {
        format!("{reported} instruments")
    };
    lines.push(format!("{head} · {partial_rows} with partial days"));
    lines
}

/// The `coverage --json` document — every partial day, uncapped (see [`MAX_PARTIAL_DAYS_SHOWN`]).
///
/// `complete` is computed from the SAME `partial_days` list the rows carry rather than from a
/// second call to the wire type's own predicate, so the flag and the array cannot disagree about
/// one instrument.
pub(crate) fn coverage_json(args: &Args, rows: &[InstrumentRow], reported: usize) -> String {
    let instruments: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "venue": r.venue,
                "name": r.name,
                "grouped": r.grouped,
                "complete": r.partial.is_empty(),
                "spanned_days": r.spanned_days,
                "kinds": r.kinds.iter().map(|k| serde_json::json!({
                    "kind": k.kind,
                    "days": k.days,
                })).collect::<Vec<_>>(),
                "partial_days": r.partial.iter().map(|p| serde_json::json!({
                    "day": p.day,
                    "start_ms": p.start_ms,
                    "date": epoch_ms_to_utc_date(p.start_ms),
                    "missing_kinds": p.missing,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let doc = serde_json::json!({
        // ⚠ DERIVED from the verb, never re-typed. These four renderers each carried the name as a
        // LITERAL, so the group split renamed `list` -> `ls` and `tape-health` -> `health` in the
        // CLI while the JSON document kept saying the old word — a document naming a spelling the
        // parser refuses. Four copies of one fact, found by the rename rather than by reading.
        "subcommand": args.sub.as_str(),
        "addr": args.addr,
        "filter": { "venue": args.filter.venue, "name": args.filter.name },
        "partial_only": args.partial_only,
        "instruments_reported": reported,
        "count": instruments.len(),
        "instruments": instruments,
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, numbers, bools and nulls; serialization is total")
}

// ─── `tape-health`: the series whose own catalog contradicts itself ─────────────────────────────
