//! The RUN: the month requests, the summary and `--json`. The rules are in `super`'s module doc.
use std::time::Instant;

use vike_datahub_client::archive::{DayClass, ImportDone, ImportPlan};
use vike_datahub_client::proto::FEATURE_ARCHIVE_IMPORT;
use vike_model::time::epoch_ms_to_utc_date;

use super::plan::confirmation;
use super::{
    Mode, MonthDone, MonthFailed, Tally, counted, day_count, month_label, refusal_lines, thousands,
};
use crate::cmd::data::hist::fetch_split::elapsed;

// ─── the month requests ───────────────────────────────────────────────────────────────────────

/// How many importable days `plan` counts in `[from, to]`.
fn importable_in(plan: &ImportPlan, from: i64, to: i64) -> u64 {
    plan.days.iter().filter(|d| d.day >= from && d.day <= to && d.class.is_importable()).count()
        as u64
}

/// The line printed before the first month is sent.
pub(crate) fn header_line(plan: &ImportPlan, months: &[(i64, i64)], mode: Mode) -> String {
    let verb = if mode == Mode::Verify { "verifying" } else { "importing" };
    let (from, to) = match (months.first(), months.last()) {
        (Some(a), Some(b)) => (a.0, b.1),
        _ => (0, 0),
    };
    format!(
        "{verb} {} {} [{} .. {}] as {} {}, one per calendar month — one line as each finishes:",
        plan.format,
        plan.dataset,
        epoch_ms_to_utc_date(from),
        epoch_ms_to_utc_date(to),
        months.len(),
        if months.len() == 1 { "request" } else { "requests" }
    )
}

/// Send one request per month through `request`, in order, calling `progress` with `header` before
/// the first and with one line as each answers. Stops at the FIRST failure — the months after it are
/// never sent — and answers what the run did before it.
///
/// `request` is the wire call (`DatahubClient::import_archive` over the run's connection, its error
/// already rendered), a parameter so the cut and the stop rule are testable without a socket.
pub(crate) fn run_months(
    months: &[(i64, i64)],
    preview: &ImportPlan,
    mode: Mode,
    header: &str,
    mut request: impl FnMut(i64, i64) -> Result<ImportDone, String>,
    mut progress: impl FnMut(&str),
) -> Result<Vec<MonthDone>, MonthFailed> {
    progress(header);
    let mut out: Vec<MonthDone> = Vec::new();
    for (n, &(from, to)) in months.iter().enumerate() {
        let started = Instant::now();
        let done = request(from, to).map_err(|error| MonthFailed {
            index: n + 1,
            count: months.len(),
            from,
            days_before: out.iter().map(|m| Tally::of_month(&m.done).decoded_days(mode)).sum(),
            error,
        })?;
        let month = MonthDone {
            from,
            to,
            done,
            preview_importable: importable_in(preview, from, to),
            elapsed: started.elapsed(),
        };
        progress(&progress_line(n + 1, months.len(), &month, mode));
        out.push(month);
    }
    Ok(out)
}

/// The line printed as month `index` (1-based) of `count` answers — the design's §7.2 drawing:
/// the month, the days it imported (or decoded), their ticks, the days refused for what is in their
/// files, and how long it took; then the first refused day's class, so the reason is on the line
/// that reports it.
pub(super) fn progress_line(index: usize, count: usize, month: &MonthDone, mode: Mode) -> String {
    let t = Tally::of_month(&month.done);
    let mut line = format!(
        "  {index}/{count}  {}  {}  {} ticks  {} refused",
        month_label(month.from),
        day_count(t.decoded_days(mode)),
        thousands(t.ticks),
        thousands(t.refused.len() as u64),
    );
    if mode == Mode::Import && t.held > 0 {
        line.push_str(&format!("  {} held", thousands(t.held)));
    }
    line.push_str(&format!("  {}", elapsed(month.elapsed)));
    if t.server_importable != month.preview_importable {
        line.push_str(&format!(
            "  (the datahub planned {} importable here, the preview {})",
            thousands(t.server_importable),
            thousands(month.preview_importable)
        ));
    }
    if let Some(first) = t.refused.first() {
        let hint = if mode == Mode::Import { " — see --dry-run --verify" } else { "" };
        line.push_str(&format!("  ({}: {}{hint})", epoch_ms_to_utc_date(first.0), first.1.class));
    }
    line
}

/// Whether a refused day is a DEFECT of the archive — something in its file or its layout — rather
/// than a day that is simply not this lane's to take. See this module's doc on the exit rung.
///
/// `plan_class` is what the SERVER's plan said of the day. ⚠ A day the plan called TOO RECENT or
/// OVERLAPPED reaches the outcome as a `Refused` row too — the import echoes its plan — and is not a
/// defect: the plan named it before anything was confirmed, and it stays the other lane's or a later
/// run's. A plan-time `Refused` is counted from the PLAN, so its echo in the outcome is not counted
/// twice.
pub(super) fn is_a_defect(plan_class: Option<&DayClass>) -> bool {
    !matches!(
        plan_class,
        Some(DayClass::TooRecent | DayClass::Overlapped { .. } | DayClass::Refused(_))
    )
}

/// The two-plans line — `Some` when the datahub's month-by-month plans counted a different number of
/// importable days than the preview did. See this module's doc: the server's count is the authority,
/// and this side only reports the difference.
pub(super) fn plans_differ_line(preview: &ImportPlan, tally: &Tally) -> Option<String> {
    let previewed = preview.importable_days() as u64;
    (previewed != tally.server_importable).then(|| {
        format!(
            "the datahub's plans differ from the preview: the preview counted {} importable, the \
             datahub counted {} as it went month by month. The datahub's count is the authority — \
             the directory or the store moved in between (a sync still writing files, or a fetch or \
             another import storing days)",
            day_count(previewed),
            day_count(tally.server_importable)
        )
    })
}

/// The lines the run ends on: the refused days, the two-plans difference when there is one, and
/// the summary — the design's `done — …` line.
pub(crate) fn summary_lines(preview: &ImportPlan, tally: &Tally, mode: Mode) -> Vec<String> {
    let mut out = Vec::new();
    if !tally.refused.is_empty() {
        out.push(format!(
            "refused — {}, and no commit key was spent for any of them:",
            day_count(tally.refused.len() as u64)
        ));
        out.extend(refusal_lines(tally.refused.iter().map(|(d, r)| (*d, r))));
    }
    out.extend(plans_differ_line(preview, tally));
    out.push(match mode {
        Mode::Verify => format!(
            "verified — {} decoded clean ({} ticks) · {} refused · nothing was written",
            day_count(tally.verified),
            thousands(tally.ticks),
            thousands(tally.refused.len() as u64)
        ),
        _ => {
            let mut line = format!(
                "done — {} imported ({} ticks, {} bars) · {} refused · {} overlapped · {} · {} \
                 already held",
                day_count(tally.imported),
                thousands(tally.ticks),
                thousands(tally.bars),
                thousands(tally.refused.len() as u64),
                thousands(tally.overlapped),
                counted(tally.gaps, "gap"),
                thousands(tally.held)
            );
            if tally.bars_topped_up > 0 {
                line.push_str(&format!(" ({} bars topped up)", thousands(tally.bars_topped_up)));
            }
            if tally.too_recent > 0 {
                line.push_str(&format!(" · {} too recent", thousands(tally.too_recent)));
            }
            line
        }
    });
    out
}

/// The sentence the run FAILS on when days were refused as defects.
pub(crate) fn refused_failure(refused: usize, mode: Mode) -> String {
    match mode {
        Mode::Import => format!(
            "{refused} day(s) of the window were refused for what is in their files (listed above) \
             — no commit key was spent for any of them, and the rest of the window was imported. \
             The same command refuses them again until the files are fixed"
        ),
        Mode::Verify => format!(
            "{refused} day(s) of the window would be refused by an import (listed above) — nothing \
             was written"
        ),
        Mode::Plan => format!(
            "{refused} day(s) of the window are refused at plan time (listed above) — nothing was \
             decoded and nothing was written"
        ),
    }
}

/// A refusal of the request as a whole, as the operator reads it.
///
/// ⚠ The capability refusal gets a line saying what to DO, because the client's own sentence says
/// what is missing and not where to fix it: the datahub at `addr` mounts no import lane, which no
/// flag on this side can change. Every other refusal — an unknown format (it names the advertised
/// ones), an unscaled instrument, a busy slot, a store failure — is the server's own sentence and is
/// passed through unchanged.
pub(crate) fn refusal(e: String, addr: &str) -> String {
    if e.contains(&format!("does not advertise `{FEATURE_ARCHIVE_IMPORT}`")) {
        format!(
            "{e}\n  what to do: the datahub at {addr} has no archive import lane. No datahub release \
             up to and including v0.1.41 has one; a newer datahub mounts it when its build carries \
             the archive formats (`--features backfill-serve`) and it runs from a project \
             directory, and its startup log says `ARCHIVE IMPORT lane mounted` or why it did not. \
             Upgrade or restart THAT datahub — nothing on this side changes the answer."
        )
    } else {
        e
    }
}

// ─── `--json` ─────────────────────────────────────────────────────────────────────────────────

/// The `--json` document — every number the human lines carry, in the wire's own words.
///
/// - `plan` is the PREVIEW, exactly as the datahub sent it (`ImportPlan`'s own serialization), so
///   a consumer reads the server's day classes and refusal tokens rather than this side's rendering.
/// - `months` holds one object per month request: its bounds, the server's importable count for it
///   beside the preview's, and the server's `outcome` verbatim. Empty for a plan-only run.
/// - `summary` is the fold of those, `null` for a plan-only run; `refused` lists the DEFECT days
///   ([`is_a_defect`]), and both importable counts are always present, so a consumer sees the
///   two-plans difference without having to recompute it.
pub(crate) fn document(
    addr: &str,
    mode: Mode,
    preview: &ImportPlan,
    months: &[MonthDone],
) -> serde_json::Value {
    let summary = (mode != Mode::Plan).then(|| {
        let t = Tally::of(months);
        serde_json::json!({
            "preview_importable": preview.importable_days(),
            "server_importable": t.server_importable,
            "imported": t.imported,
            "verified": t.verified,
            "ticks": t.ticks,
            "bars": t.bars,
            "held": t.held,
            "bars_topped_up": t.bars_topped_up,
            "overlapped": t.overlapped,
            "too_recent": t.too_recent,
            "gaps": t.gaps,
            "refused": t.refused.iter().map(|(d, r)| serde_json::json!({
                "day": d,
                "date": epoch_ms_to_utc_date(*d),
                "class": r.class,
                "detail": r.detail,
            })).collect::<Vec<_>>(),
        })
    });
    serde_json::json!({
        "addr": addr,
        "format": preview.format,
        "dataset": preview.dataset,
        "mode": mode.as_str(),
        "confirmation": (mode == Mode::Import).then(|| confirmation(preview)),
        "plan": preview,
        "months": months.iter().map(|m| serde_json::json!({
            "month": month_label(m.from),
            "from_day": m.from,
            "to_day": m.to,
            "server_importable": m.done.plan.importable_days(),
            "preview_importable": m.preview_importable,
            "elapsed_ms": u64::try_from(m.elapsed.as_millis()).unwrap_or(u64::MAX),
            "outcome": m.done.outcome,
        })).collect::<Vec<_>>(),
        "summary": summary,
    })
}
