//! `data hist import FORMAT DATASET` — read ONE dataset of a vendor ARCHIVE, which the user put on
//! the DATAHUB's own box, into that datahub's store. Every line this verb prints, the month cut and
//! the summary arithmetic live here, as pure functions over the wire's own plan and outcome, so the
//! wording is tested without a socket; `crate::cmd::data`'s `execute_import` is the dial, the
//! requests and the prints.
//!
//! The approved design is `docs/superpowers/specs/2026-09-30-archive-import-lane-design.md` — task
//! T5 of its section 9, the verb of its section 7 — and
//! `docs/decisions/0100-the-archive-import-reads-the-datahubs-own-box-and-a-day-has-one-owner.md`
//! decides who may send it and where the files are read from. The wire is
//! `vike_datahub_client::archive`'s: its module doc carries the request, the answer and the
//! client-checkable rules, and nothing of it is restated here.
//!
//! # The flow, and why each step sits where it does
//!
//! 1. **ONE plan-only request** fetches the SERVER's plan for the whole window, and it is printed
//!    first, always ([`plan_lines`]). A plan-only request decodes nothing, so the wire's day cap
//!    (`vike_datahub_client::archive::IMPORT_MAX_DAYS`) does not bound it.
//! 2. **`--dry-run` stops there.** `--dry-run --verify` goes on to step 4 with `verify` set: every
//!    importable file is decoded and checked, and nothing is written.
//! 3. **An import asks for the typed confirmation [`confirmation`]** — `import N days`, N being the
//!    plan's own `ImportPlan::importable_days` — unless `--yes` was given. A non-terminal without
//!    `--yes` is refused before the socket, on the usage rung, as `rm` refuses one.
//! 4. **ONE REQUEST PER CALENDAR MONTH** ([`months_of`], sent by [`run_months`]), each with a
//!    progress line on stderr as it answers. The design's §7.3 argues the month: about 22 files, so
//!    a line lands every few seconds; never past the wire's 31-day cap; and a killed session is
//!    resumed by running the same command, because every day it finished comes back HELD.
//! 5. **A summary** ([`summary_lines`]).
//!
//! # Two plans, one authority
//!
//! Each month request makes the server plan its month AGAIN, and that plan is what it acts on: the
//! directory may have gained files since the preview (a sync still running) and the store may have
//! gained days (a `data hist fetch`, another import). So the preview's import count and the sum of
//! the month plans' counts can differ, and when they do the summary prints BOTH
//! ([`plans_differ_line`]) and names the server's as the authority. Nothing on this side reconciles
//! the two, because only the server can.
//!
//! # Which months are asked for
//!
//! The months of the window the SERVER resolved (an omitted `--from`/`--to` is the dataset's own
//! first or last day), clipped to the dataset's own first and last daily file: a month outside that
//! span holds no file to import. Every month inside it is asked for, held ones included, because an
//! import TOPS UP a held day's missing bar intervals from the stored ticks — that is how a run killed
//! between a day's ticks and its bars is finished — and only the server knows which are missing.
//!
//! # The exit rung
//!
//! `0` when every day with a file in the window was imported, verified, already held, or is not this
//! lane's to take (too recent yet, or overlapped by an earlier fetch's key) — the plan names those
//! before anything is confirmed. `1` when a day was refused for what is IN its file or its layout (a
//! mixed layout, an ambiguous time base, a header past a cap, …): the summary lists each one, no
//! commit key was spent for it, and the same command will refuse it again until the file is fixed.
//! [`is_a_defect`] is that split. `1` also when the dataset's directory is absent or unreadable on
//! the datahub's box, and for every refusal from a box that answered — `crate::cmd::data`'s module
//! doc carries that ladder.

use std::collections::BTreeMap;
use std::time::Duration;

use vike_datahub_client::archive::{
    DayClass, DayRefusal, DayResult, ImportDone, ImportPlan, validate_import_bars,
};
use vike_model::{
    MS_PER_DAY, parse_date_label, parse_ymd,
    time::{civil_from_days, days_from_civil, epoch_ms_to_utc_date},
};

pub(super) mod plan;
pub(super) mod run;

#[cfg(doc)]
use self::plan::{confirmation, months_of, plan_lines};
use self::run::is_a_defect;
#[cfg(doc)]
use self::run::{plans_differ_line, run_months, summary_lines};

/// The bar intervals an import derives per imported day when `--bars` is not given — the design's
/// §3.5 default.
pub(super) const DEFAULT_BARS: &str = "1m";

/// `--bars none`: derive no bars, store the ticks alone. A WORD rather than an empty value, because
/// `--bars ""` is far likelier to be a shell-quoting accident than a decision.
pub(super) const NO_BARS: &str = "none";

/// How many days one human-rendered list shows before it says how many it withheld — the overlapped
/// days, the refused days and the gaps. A twenty-year dataset can carry hundreds of gaps, and a plan
/// that scrolled them off the top of a terminal would bury the line asking for a confirmation.
///
/// ⚠ It bounds the HUMAN rendering only: the `--json` document carries the server's whole plan.
pub(super) const MAX_LISTED: usize = 10;

/// The ONE format this side has a SYNC HINT for — the line printed when a dataset's directory is
/// absent. It is a key into a hint, never a roster: a format the datahub advertises and this binary
/// has no hint for imports exactly the same, and its absent-directory answer is the generic one. The
/// server's registry is the authority for the id (`crates/vike-datahub/src/import/formats.rs`'s
/// `DUKASCOPY_BI5`), and `the_sync_hint_names_the_servers_own_format_id` holds the two equal.
pub(super) const DUKASCOPY_FORMAT: &str = "dukascopy-bi5";

/// What one invocation does: show the plan, decode without writing, or import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// `--dry-run` — the plan, and nothing after it.
    Plan,
    /// `--dry-run --verify` — the plan, then every importable file DECODED, month by month, with
    /// nothing written.
    Verify,
    /// No `--dry-run` — the plan, the confirmation, then the import, month by month.
    Import,
}

impl Mode {
    /// The mode a command line asks for. `verify` without `dry_run` never reaches here: the parser
    /// refuses it, as the wire does (`vike_datahub_client::archive::validate_import_window`).
    pub(super) fn of(dry_run: bool, verify: bool) -> Self {
        match (dry_run, verify) {
            (true, true) => Mode::Verify,
            (true, false) => Mode::Plan,
            (false, _) => Mode::Import,
        }
    }

    /// Whether a request of this mode may WRITE the store — exactly an import. The one place the
    /// month requests' `dry_run` is decided, so no request a dry run sends can write.
    pub(super) fn writes(self) -> bool {
        self == Mode::Import
    }

    /// The `mode` field of the `--json` document.
    fn as_str(self) -> &'static str {
        match self {
            Mode::Plan => "plan",
            Mode::Verify => "verify",
            Mode::Import => "import",
        }
    }
}

// ─── the grammar's two parsers ────────────────────────────────────────────────────────────────

/// Parse a `--from`/`--to` value into the epoch-ms START of an inclusive UTC day.
///
/// ⚠ **Days are this verb's unit, and that is the difference from `fetch`.** `data hist fetch`'s
/// `--to` is an INSTANT; here `--from 2024-01-15 --to 2024-01-15` is ONE day, the whole of it. So
/// the value is a day label — `YYYY-MM-DD`, or the epoch-ms of a UTC midnight — and anything else is
/// refused rather than rounded: an instant inside a day would have to be read as either that day or
/// the next, and an import that guessed wrong could not be undone short of deleting the series.
///
/// ⚠ **A date that does not exist is refused too.** `vike_model::parse_date_label` reads
/// `2024-02-30` as 1 March, which is right for a fetch's instant and wrong here: the operator named a
/// day, and silently importing a different one is the class of guess this lane refuses everywhere.
pub(super) fn parse_day(flag: &str, raw: &str) -> Result<i64, String> {
    let ms = parse_date_label(raw).map_err(|e| {
        format!(
            "{flag} {raw:?} is not a day this side can read ({e}). An import's bounds are \
             inclusive UTC DAYS: YYYY-MM-DD, such as 2024-01-15"
        )
    })?;
    if ms.rem_euclid(MS_PER_DAY) != 0 {
        return Err(format!(
            "{flag} {raw:?} is not the START of a UTC day. An import's bounds are inclusive DAYS — \
             --from 2024-01-15 --to 2024-01-15 imports that one day, whole — so an instant inside \
             a day would have to be read as one of two days. Name the day: YYYY-MM-DD"
        ));
    }
    if raw.trim().parse::<i64>().is_err() {
        let (y, m, d) = parse_ymd(raw)?;
        if civil_from_days(days_from_civil(y, m, d)) != (y, m, d) {
            return Err(format!(
                "{flag} {raw:?} is not a calendar date — that month has no such day, and reading it \
                 as the next real one would import a day you did not name"
            ));
        }
    }
    Ok(ms)
}

/// Parse `--bars`: absent is [`DEFAULT_BARS`], [`NO_BARS`] is none at all, anything else a
/// comma-separated list held to the wire's own rules (`vike_datahub_client::archive`'s
/// `validate_import_bars` — a step that divides a UTC day, canonical spelling, no repeat, at most
/// four) HERE, so a mistake is a usage error rather than a round trip.
pub(super) fn parse_bars(raw: Option<&str>) -> Result<Vec<String>, String> {
    let Some(raw) = raw else { return Ok(vec![DEFAULT_BARS.to_string()]) };
    let raw = raw.trim();
    if raw == NO_BARS {
        return Ok(Vec::new());
    }
    if raw.is_empty() {
        return Err(format!(
            "--bars was given an EMPTY value. Name the intervals to derive per imported day \
             (`--bars 1m,5m`), or `--bars {NO_BARS}` to store the ticks alone; omit the flag for \
             the default, {DEFAULT_BARS}"
        ));
    }
    let bars: Vec<String> = raw.split(',').map(|b| b.trim().to_string()).collect();
    if bars.iter().any(|b| b == NO_BARS) {
        return Err(format!(
            "--bars {raw:?} names `{NO_BARS}` beside an interval. `--bars {NO_BARS}` stands alone: \
             it means NO bars, and a list that also names one says two things at once"
        ));
    }
    if let Some(blank) = bars.iter().position(String::is_empty) {
        return Err(format!(
            "--bars {raw:?} has an empty entry at position {blank}. Separate the intervals with \
             single commas: `--bars 1m,5m`"
        ));
    }
    validate_import_bars(&bars).map_err(|e| format!("--bars: {e}"))?;
    Ok(bars)
}

/// `YYYY-MM` of the month holding `day`.
fn month_label(day: i64) -> String {
    let (y, m, _) = civil_from_days(day.div_euclid(MS_PER_DAY));
    format!("{y:04}-{m:02}")
}

/// `n` with a comma between every three digits — `1,002,334`. The plan and the progress lines carry
/// tick counts in the millions, and an ungrouped one is a number nobody reads at a glance.
pub(super) fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `n` and `noun`, the noun inflected — "1 day" / "3 days", "1 weekday" / "41 weekdays".
fn counted(n: u64, noun: &str) -> String {
    format!("{} {noun}{}", thousands(n), if n == 1 { "" } else { "s" })
}

/// "1 day" / "3 days".
fn day_count(n: u64) -> String {
    counted(n, "day")
}

/// How the plan's days fall into the design's §4.2 classes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct ClassCounts {
    /// `Free` + `Supersede` — what an import stores. The confirmation's N.
    pub(super) importable: u64,
    /// Of those, the `Supersede` days.
    pub(super) supersede: u64,
    pub(super) held_by_archive: u64,
    pub(super) held_by_http: u64,
    pub(super) too_recent: u64,
    /// The overlapped days. Which keys meet each is in the server's plan, and so in `--json`.
    pub(super) overlapped: Vec<i64>,
    /// Days refused AT PLAN TIME — a mixed layout, a header past a cap.
    pub(super) refused: Vec<(i64, DayRefusal)>,
    /// Importable days whose header declared a tick count, and the ticks they declared.
    pub(super) declared_days: u64,
    pub(super) declared_ticks: u64,
}

impl ClassCounts {
    /// Fold one plan's days.
    pub(super) fn of(plan: &ImportPlan) -> Self {
        let mut c = ClassCounts::default();
        for d in &plan.days {
            match &d.class {
                DayClass::Free => c.importable += 1,
                DayClass::Supersede { .. } => {
                    c.importable += 1;
                    c.supersede += 1;
                }
                DayClass::HeldByArchive => c.held_by_archive += 1,
                DayClass::HeldByHttp => c.held_by_http += 1,
                DayClass::TooRecent => c.too_recent += 1,
                DayClass::Overlapped { .. } => c.overlapped.push(d.day),
                DayClass::Refused(r) => c.refused.push((d.day, r.clone())),
            }
            if d.class.is_importable()
                && let Some(ticks) = d.declared_ticks
            {
                c.declared_days += 1;
                c.declared_ticks += ticks;
            }
        }
        c
    }

    /// Held by either lane.
    pub(super) fn held(&self) -> u64 {
        self.held_by_archive + self.held_by_http
    }
}

/// One line per refused day: its date, its class token and the server's sentence.
fn refusal_lines<'r>(days: impl Iterator<Item = (i64, &'r DayRefusal)>) -> Vec<String> {
    let all: Vec<(i64, &DayRefusal)> = days.collect();
    let mut out: Vec<String> = all
        .iter()
        .take(MAX_LISTED)
        .map(|(d, r)| {
            format!("              {}  {} — {}", epoch_ms_to_utc_date(*d), r.class, r.detail)
        })
        .collect();
    if all.len() > MAX_LISTED {
        out.push(format!(
            "              … and {} more (the --json document carries every one)",
            all.len() - MAX_LISTED
        ));
    }
    out
}

/// One month that answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MonthDone {
    /// The month's first and last day, as requested.
    pub(super) from: i64,
    pub(super) to: i64,
    /// The datahub's answer — ITS plan of this month, and what it did.
    pub(super) done: ImportDone,
    /// How many importable days the PREVIEW counted in this month, so a month the server re-planned
    /// differently can say so on its own line.
    pub(super) preview_importable: u64,
    pub(super) elapsed: Duration,
}

/// The first month that failed, and what the run had done before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MonthFailed {
    /// 1-based.
    pub(super) index: usize,
    pub(super) count: usize,
    pub(super) from: i64,
    /// Days the months BEFORE this one imported (or, for a verify, decoded).
    pub(super) days_before: u64,
    /// The request's own failure text, as the caller rendered it.
    pub(super) error: String,
}

impl MonthFailed {
    /// The sentence the run ends on — what failed, what stays stored, and how to resume.
    pub(super) fn message(&self, mode: Mode) -> String {
        let (verb, stays) = match mode {
            Mode::Import => ("imported", ", which stay stored"),
            _ => ("decoded", ""),
        };
        let before = match self.index - 1 {
            0 => format!("it was the first, so this run {verb} nothing"),
            1 => format!("the month before it {verb} {}{stays}", day_count(self.days_before)),
            n => format!("the {n} months before it {verb} {}{stays}", day_count(self.days_before)),
        };
        let resume = match mode {
            Mode::Import => {
                "Re-run the same command to resume: every day already imported comes back HELD in \
                 the next plan and is not decoded again."
            }
            _ => "Nothing was written. Re-run the same command to verify again.",
        };
        format!(
            "month {} of {} ({}) failed; {before}. {resume}\n  {}",
            self.index,
            self.count,
            month_label(self.from),
            self.error
        )
    }
}

/// What the months did, folded — the summary's numbers and the exit rung's evidence.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Tally {
    /// Days decoded and stored.
    pub(super) imported: u64,
    /// Days decoded and checked by a verify.
    pub(super) verified: u64,
    /// Ticks stored (an import) or decoded (a verify).
    pub(super) ticks: u64,
    /// Bars written for imported days.
    pub(super) bars: u64,
    /// Days the server's plans called held, and the bars a top-up wrote for them.
    pub(super) held: u64,
    pub(super) bars_topped_up: u64,
    pub(super) overlapped: u64,
    pub(super) too_recent: u64,
    pub(super) gaps: u64,
    /// Importable days, by the server's month plans — compared with the preview's.
    pub(super) server_importable: u64,
    /// Days refused as DEFECTS ([`is_a_defect`]), plan-time and decode-time, in day order.
    pub(super) refused: Vec<(i64, DayRefusal)>,
}

impl Tally {
    /// One month's answer.
    pub(super) fn of_month(done: &ImportDone) -> Self {
        let plan = &done.plan;
        let c = ClassCounts::of(plan);
        let mut t = Tally {
            held: c.held(),
            overlapped: c.overlapped.len() as u64,
            too_recent: c.too_recent,
            gaps: plan.gaps.len() as u64,
            server_importable: c.importable,
            refused: c.refused.clone(),
            ..Tally::default()
        };
        let class: BTreeMap<i64, &DayClass> = plan.days.iter().map(|d| (d.day, &d.class)).collect();
        for row in done.outcome.iter().flat_map(|o| &o.days) {
            match &row.result {
                DayResult::Imported { ticks, bars } => {
                    t.imported += 1;
                    t.ticks += ticks;
                    t.bars += bars.iter().map(|b| b.rows).sum::<u64>();
                }
                DayResult::ToppedUp { bars } => {
                    t.bars_topped_up += bars.iter().map(|b| b.rows).sum::<u64>();
                }
                DayResult::Verified { ticks } => {
                    t.verified += 1;
                    t.ticks += ticks;
                }
                DayResult::Refused(r) => {
                    if is_a_defect(class.get(&row.day).copied()) {
                        t.refused.push((row.day, r.clone()));
                    }
                }
            }
        }
        t.refused.sort_by_key(|(d, _)| *d);
        t
    }

    /// Every month's answer, summed.
    pub(super) fn of(months: &[MonthDone]) -> Self {
        let mut all = Tally::default();
        for m in months {
            let t = Tally::of_month(&m.done);
            all.imported += t.imported;
            all.verified += t.verified;
            all.ticks += t.ticks;
            all.bars += t.bars;
            all.held += t.held;
            all.bars_topped_up += t.bars_topped_up;
            all.overlapped += t.overlapped;
            all.too_recent += t.too_recent;
            all.gaps += t.gaps;
            all.server_importable += t.server_importable;
            all.refused.extend(t.refused);
        }
        all
    }

    /// The days a run of `mode` decoded — imported, or verified.
    pub(super) fn decoded_days(&self, mode: Mode) -> u64 {
        if mode == Mode::Verify { self.verified } else { self.imported }
    }
}

#[cfg(test)]
mod tests;
