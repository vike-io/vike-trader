//! `vike-cli report --compare <A> <B>` — **two sources' results side by side**, A the reference and B held
//! against it: `vike_analytics::results_comparison::TradingResultsComparison`, the ONE comparison the owner
//! ruled (2026-11-17), which the Studio's "Compare all" and a forward test's end render too.
//!
//! # Each side is the source's own results
//!
//! A `run:` backtest is its stored `report.json` (`TradingResults::read_stored`, its period the stored
//! curve's first sample to its last bar's CLOSE, `super::stored`'s `stored_period`); a `run:` forward run and an `account:` are folded from the datahub's
//! fills exactly as `report` folds one ([`vike_client::account_report`]). Nothing is recomputed from a stored run.
//!
//! # The alignment (the comparison aligns nothing itself; this caller does)
//!
//! - **The window.** An `account:` side reads `--from`/`--to` when they are given, else the period of the
//!   run on the other side — a backtest's own, or a forward run's window — which is the forward test held
//!   against its backtest. Two accounts with no window read all of their history. A run's period is never
//!   cut (its curve may be thinned): `--from`/`--to` bound an account only.
//! - **The capital.** A live side's realized curve starts from `--cash` when it is given, else from the
//!   stored backtest's starting equity (its curve's first sample), so the two return ratios are of one
//!   capital; with no stored side, the default. Each side says its starting capital and where it came from.
//!   Two stored backtests have no live side, so a live knob (`--cash`, …) is refused on them.
//! - **A stored run that kept no curve** (no `series.json`) gives neither: it says what it lacks, and a live
//!   side that would align to it is refused unless `--from`/`--to` and `--cash` name them.

use serde_json::{Value, json};
use vike_analytics::results::TradingResults;
use vike_analytics::results_comparison::TradingResultsComparison;
use vike_analytics::{TradingPeriod, TradingRecordSource};
use vike_client::account_report::{self, LiveFold, LiveQuery};
use vike_data::TimestampRange;

use super::comparison_schema::{COMPARISON_SCHEMA, DOCUMENT_ID};
use super::{
    Args, Context, Source, forward_query, live_fold, live_side, refuse_a_live_knob, stored,
    write_html,
};
use crate::exit::{CliError, CommandResult};

/// One side as the line named it, read as far as it can be before the other side is known.
enum Side {
    /// A stored backtest: its results, its curve's first sample (the capital a live side aligns to), what its
    /// stored curve lacks (so its period or its capital is unknown), and what it says of its period and capital.
    Stored {
        run_id: String,
        results: Box<TradingResults>,
        first_equity: Option<f64>,
        gap: Option<String>,
        notes: Vec<String>,
    },
    /// A forward run: its account over its own window, kept to its mount.
    Forward { run_id: String, query: LiveQuery },
    /// An account: its window is the comparison's to decide.
    Account(LiveQuery),
}

impl Side {
    /// The period a live `account:` side aligns to: a stored run's, else a forward run's window.
    fn reference_period(&self) -> Option<TradingPeriod> {
        match self {
            Side::Stored { results, .. } => results.period,
            Side::Forward { query, .. } => {
                Some(TradingPeriod { from_ms: query.window.start?, to_ms: query.window.end? })
            }
            Side::Account(_) => None,
        }
    }
}

/// Read what a source names, as far as it can be read alone.
fn resolve(ctx: &Context<'_>, source: &Source) -> CommandResult<Side> {
    match source {
        Source::Run(selector) => {
            let run =
                stored::resolve_run(ctx.runs_root.as_deref(), ctx.marks_root.as_deref(), selector)?;
            if run.manifest.kind == vike_runs::FORWARD_RUN_KIND {
                return Ok(Side::Forward {
                    query: forward_query(&run, ctx.now_ms)?,
                    run_id: run.run_id,
                });
            }
            stored_side(&run)
        }
        Source::Account(book) => Ok(Side::Account(LiveQuery {
            account: account_report::account_ref(&book.venue, book.label.clone())?,
            window: TimestampRange::all(),
            mount_id: None,
            title: None,
        })),
    }
}

/// A stored backtest's results: its `report.json`, typed, with the source and the period its bytes do not
/// carry. A run with no report, or a report that is not one (a walk-forward run's), is refused by name.
fn stored_side(run: &vike_runs::ScannedRun) -> CommandResult<Side> {
    let Some(path) = &run.report else {
        return Err(CliError::failed(format!(
            "{} has no report.json (it stopped before writing one), so it holds no results to compare",
            run.run_id
        )));
    };
    let text = std::fs::read_to_string(path)
        .map_err(|e| CliError::failed(format!("cannot read {}: {e}", path.display())))?;
    // The curve is optional: a run that kept none states no period and no starting capital, and says which.
    let both = "so its period and starting capital are unknown";
    let (series, gap) = match vike_runs::read_series(&run.dir) {
        Ok(s) if s.equity.is_empty() => {
            (None, Some(format!("its series.json keeps no equity sample, {both}")))
        }
        Ok(s) if s.equity_timestamp.is_empty() => {
            (Some(s), Some("its series.json keeps no timestamps, so its period is unknown".into()))
        }
        Ok(s) => (Some(s), None),
        Err(vike_runs::RunReadError::Missing { .. }) => {
            (None, Some(format!("it stored no series.json, {both}")))
        }
        Err(e) => (None, Some(format!("its series.json cannot be read ({e}), {both}"))),
    };
    let timestamps = series.as_ref().map_or(&[][..], |s| s.equity_timestamp.as_slice());
    // To the last bar's CLOSE: an account aligned to it reads the fills of that bar too.
    let (period, period_note) = stored::stored_period(run, timestamps);
    let results = TradingResults::read_stored(&text, TradingRecordSource::default(), period)
        .map_err(|e| {
            CliError::failed(format!(
                "{} cannot be compared: {e} (a walk-forward run's report holds steps, not one result)",
                run.run_id
            ))
        })?;
    let first_equity = series.and_then(|s| s.equity.first().copied());
    let seed =
        first_equity.map(|e| format!("starting capital {e} (its stored curve's first equity)"));
    Ok(Side::Stored {
        run_id: run.run_id.clone(),
        results: Box::new(results),
        first_equity,
        notes: gap.iter().cloned().chain(period_note).chain(seed).collect(),
        gap,
    })
}

/// **A live side aligned to a stored run that cannot give what it aligns to is REFUSED**, naming what the run
/// lacks, never aligned to all of history or to the default capital: an account side with no `--from`/`--to`
/// takes the run's period, a live side with no `--cash` its starting equity.
fn refuse_an_unalignable_side(a: &Args, sides: &[Side]) -> CommandResult<()> {
    let accounts = sides.iter().any(|s| matches!(s, Side::Account(_)));
    let live = accounts || sides.iter().any(|s| matches!(s, Side::Forward { .. }));
    for side in sides {
        let Side::Stored { run_id, results, first_equity, gap: Some(gap), .. } = side else {
            continue;
        };
        if accounts && a.from.is_none() && a.to.is_none() && results.period.is_none() {
            return Err(CliError::failed(format!(
                "the account side cannot be aligned to run:{run_id}: {gap}. Name its window with \
                 --from/--to."
            )));
        }
        if live && a.cash.is_none() && first_equity.is_none() {
            return Err(CliError::failed(format!(
                "the live side's starting capital cannot be aligned to run:{run_id}: {gap}. Name it \
                 with --cash."
            )));
        }
    }
    Ok(())
}

/// How the live sides fold: the line's knobs, the capital `--cash`, else the stored side's first equity (named),
/// else the default.
fn aligned_fold(a: &Args, sides: &[Side]) -> LiveFold {
    let stored_capital = sides.iter().find_map(|s| match s {
        Side::Stored { run_id, first_equity: Some(e), .. } => Some((run_id, *e)),
        _ => None,
    });
    match (a.cash, stored_capital) {
        (None, Some((run_id, cash))) => LiveFold {
            cash,
            cash_from: format!("run:{run_id}'s stored curve's first equity"),
            ..live_fold(a)
        },
        _ => live_fold(a),
    }
}

/// One live side folded, and its notes (what it counted, left out or could not convert): the comparison names
/// each side by its `source_label()` and says each note on a line of its own.
fn live_results(
    ctx: &Context<'_>,
    query: &LiveQuery,
    fold: &LiveFold,
) -> CommandResult<(TradingResults, Vec<String>)> {
    let side = live_side(ctx, query, fold)?;
    Ok((side.results, side.notes))
}

/// One side as the comparison names it: the source as read (`run:<id>`, `account:<venue>/<LABEL>`) and its notes.
pub(crate) struct ComparisonSide {
    pub source: String,
    pub notes: Vec<String>,
}

/// **`report --compare <A> <B>`**: both sides read and aligned (this module's doc), then the comparison as
/// text, as its document (`--json`), and as an HTML page (`--html`).
pub(super) fn run_compare(
    ctx: &Context<'_>,
    a: &Args,
    first: &Source,
    second: &Source,
) -> CommandResult<String> {
    let sides = [resolve(ctx, first)?, resolve(ctx, second)?];
    let reference = sides.iter().find_map(Side::reference_period);
    if let [Side::Stored { run_id: x, .. }, Side::Stored { run_id: y, .. }] = &sides {
        refuse_a_live_knob(a, &format!("Both sides ({x} and {y}) are stored backtests."))?;
    }
    refuse_an_unalignable_side(a, &sides)?;
    let fold = aligned_fold(a, &sides);
    let asked =
        (a.from.is_some() || a.to.is_some()).then_some(TimestampRange { start: a.from, end: a.to });
    let mut named = Vec::with_capacity(2);
    let mut results = Vec::with_capacity(2);
    for side in sides {
        let (source, (side_results, notes)) = match side {
            Side::Stored { run_id, results, notes, .. } => {
                (format!("run:{run_id}"), (*results, notes))
            }
            Side::Forward { run_id, query } => {
                (format!("run:{run_id}"), live_results(ctx, &query, &fold)?)
            }
            Side::Account(mut query) => {
                query.window = asked
                    .or_else(|| reference.map(|p| TimestampRange::of(p.from_ms, p.to_ms)))
                    .unwrap_or_default();
                let source = format!("account:{}/{}", query.account.venue, query.account.label);
                (source, live_results(ctx, &query, &fold)?)
            }
        };
        named.push(ComparisonSide { source, notes });
        results.push(side_results);
    }
    let mut results = results.into_iter();
    let (Some(reference_side), Some(held)) = (results.next(), results.next()) else {
        return Err(CliError::failed("a comparison needs two sides"));
    };
    let comparison = TradingResultsComparison::of(reference_side, held);
    // Each side's notes, a line each: `a: fees in other assets: 0.0123 BNB (not converted; …)`.
    let lines: Vec<String> = ["a", "b"]
        .iter()
        .zip(&named)
        .flat_map(|(letter, side)| side.notes.iter().map(move |note| format!("{letter}: {note}")))
        .collect();
    let text = if a.json {
        let doc = serde_json::to_string_pretty(&document(&named, &comparison))
            .map_err(|e| CliError::failed(format!("cannot render the comparison ({e})")))?;
        format!("{doc}\n")
    } else {
        let mut text = comparison.to_string();
        for line in &lines {
            text.push_str(&format!("{line}\n"));
        }
        text
    };
    if let Some(path) = &a.html {
        let captions: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_html(path, &vike_analytics::render_comparison_html(&comparison, &captions))?;
    }
    Ok(text)
}

/// The comparison document, with the FIXED key set `crate::cmd::report::comparison_schema`'s
/// `TOP_LEVEL_KEYS` publishes. A non-finite value is `null` (`serde_json`'s rule for an `f64`). A side's
/// `label` is the words the text and the HTML head it with: its `source_label()`, else its name; its `notes`
/// what it counted, left out or could not convert.
pub(crate) fn document(sides: &[ComparisonSide], comparison: &TradingResultsComparison) -> Value {
    let side = |named: Option<&ComparisonSide>, results: &TradingResults| {
        let label = results.source_label().or_else(|| results.name.clone()).unwrap_or_default();
        json!({
            "source": named.map(|n| n.source.clone()).unwrap_or_default(),
            "label": label,
            "period": results.period.map(|p| json!({ "from_ms": p.from_ms, "to_ms": p.to_ms })),
            "invalid": results.invalid,
            "notes": named.map(|n| n.notes.clone()).unwrap_or_default(),
        })
    };
    let rows: Vec<Value> = comparison
        .rows()
        .iter()
        .map(|r| json!({ "metric": r.spec.id, "a": r.a, "b": r.b, "difference": r.difference }))
        .collect();
    json!({
        "document": DOCUMENT_ID,
        "schema": COMPARISON_SCHEMA,
        "a": side(sides.first(), &comparison.a),
        "b": side(sides.get(1), &comparison.b),
        "rows": rows,
    })
}

#[path = "comparison_tests.rs"]
#[cfg(test)]
mod comparison_tests;
