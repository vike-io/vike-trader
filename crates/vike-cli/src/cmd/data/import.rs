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
use std::time::{Duration, Instant};

use vike_datahub_client::archive::{
    DatasetDir, DayClass, DayRefusal, DayResult, ImportDone, ImportPlan, validate_import_bars,
};
use vike_datahub_client::proto::FEATURE_ARCHIVE_IMPORT;
use vike_model::{
    MS_PER_DAY, civil_from_days, days_from_civil, epoch_ms_to_utc_date, parse_date_label, parse_ymd,
};

use super::fetch_split::elapsed;

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

// ─── the month cut ────────────────────────────────────────────────────────────────────────────

/// The calendar months of the inclusive day window `[from, to]` (both UTC midnights), as inclusive
/// `(first day, last day)` pairs — the first and last keeping the window's own bounds.
///
/// ⚠ **The pieces TILE the window**: consecutive pieces are one day apart, so no day is in two
/// requests and none in neither, and every interior cut is a 1st of the month. A request is then at
/// most 31 days, inside the wire's `IMPORT_MAX_DAYS`, which is the whole reason a month is the unit.
pub(super) fn months(from: i64, to: i64) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    let mut at = from;
    while at <= to {
        let (y, m, _) = civil_from_days(at.div_euclid(MS_PER_DAY));
        let (ny, nm) = if m == 12 { (y.saturating_add(1), 1) } else { (y, m + 1) };
        let Some(next) = days_from_civil(ny, nm, 1).checked_mul(MS_PER_DAY) else {
            out.push((at, to));
            break;
        };
        out.push((at, (next - MS_PER_DAY).min(to)));
        at = next;
    }
    out
}

/// The months to request for `plan` — the window the SERVER resolved, clipped to the dataset's own
/// first and last daily file (see this module's doc). Empty when the dataset holds no daily file or
/// the two do not meet.
pub(super) fn months_of(plan: &ImportPlan) -> Vec<(i64, i64)> {
    let inv = &plan.inventory;
    match (plan.from_day, plan.to_day, inv.first_day, inv.last_day) {
        (Some(from), Some(to), Some(first), Some(last)) if from.max(first) <= to.min(last) => {
            months(from.max(first), to.min(last))
        }
        _ => Vec::new(),
    }
}

/// `YYYY-MM` of the month holding `day`.
fn month_label(day: i64) -> String {
    let (y, m, _) = civil_from_days(day.div_euclid(MS_PER_DAY));
    format!("{y:04}-{m:02}")
}

// ─── the plan ─────────────────────────────────────────────────────────────────────────────────

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

/// A byte count in decimal units, one decimal place — the store growth an operator sizes a disk by.
fn human_bytes(bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 { format!("{value:.0} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

/// `n` and `noun`, the noun inflected — "1 day" / "3 days", "1 weekday" / "41 weekdays".
fn counted(n: u64, noun: &str) -> String {
    format!("{} {noun}{}", thousands(n), if n == 1 { "" } else { "s" })
}

/// "1 day" / "3 days".
fn day_count(n: u64) -> String {
    counted(n, "day")
}

/// Up to [`MAX_LISTED`] dates, and how many more were withheld.
fn date_list(days: &[i64]) -> String {
    let shown: Vec<String> =
        days.iter().take(MAX_LISTED).map(|d| epoch_ms_to_utc_date(*d)).collect();
    let more = days.len().saturating_sub(MAX_LISTED);
    if more == 0 {
        shown.join(" ")
    } else {
        format!("{} … and {more} more (the --json document carries every one)", shown.join(" "))
    }
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

/// The plan as the operator reads it — the design's §7.2 drawing, every line a fact the SERVER
/// sent. The first line names the datahub and the second the directory ON ITS BOX, because a
/// tunnelled datahub is `127.0.0.1` too and that path is how an operator learns which box the files
/// must be on.
///
/// For an absent or unreadable directory it stops after the directory line; [`dir_lines`] says
/// what to do about it.
pub(super) fn plan_lines(plan: &ImportPlan, addr: &str) -> Vec<String> {
    let mut out = vec![format!("{} · {} — datahub at {addr}", plan.format, plan.dataset)];
    match &plan.dir {
        DatasetDir::Present => {
            out.push(format!("  server dir  {}   (on the DATAHUB's box)", plan.server_dir));
        }
        DatasetDir::Absent => {
            out.push(format!("  server dir  {}   NOT FOUND on the datahub's box", plan.server_dir));
            return out;
        }
        DatasetDir::Unreadable { why } => {
            out.push(format!(
                "  server dir  {}   EXISTS, and the datahub cannot read it: {why}",
                plan.server_dir
            ));
            return out;
        }
    }
    let inv = &plan.inventory;
    let span = match (inv.first_day, inv.last_day) {
        (Some(a), Some(b)) => {
            format!(" {} .. {}", epoch_ms_to_utc_date(a), epoch_ms_to_utc_date(b))
        }
        _ => String::new(),
    };
    out.push(format!(
        "  files       {} daily files{span} · {} in another layout (counted, not imported) · {} \
         other objects (not read)",
        thousands(inv.daily_files),
        day_count(inv.other_layout_days.len() as u64),
        thousands(inv.other_objects)
    ));
    if let Some(first) = inv.skipped.first() {
        out.push(format!(
            "  skipped     {} entries the walk would not follow or open (a symlink, a FIFO, a hard \
             link …) — the first is {} ({:?})",
            inv.skipped.len(),
            first.path,
            first.class
        ));
    }
    match (plan.from_day, plan.to_day) {
        (Some(from), Some(to)) => out.push(format!(
            "  window      {} .. {} → {} daily files",
            epoch_ms_to_utc_date(from),
            epoch_ms_to_utc_date(to),
            thousands(plan.days.len() as u64)
        )),
        _ => out.push("  window      none — the dataset holds no daily file".to_string()),
    }
    let c = ClassCounts::of(plan);
    if c.held() > 0 {
        out.push(format!(
            "  held        {} ({} by this lane · {} by the HTTP lane) — not read again; their \
             bars are topped up if missing",
            day_count(c.held()),
            thousands(c.held_by_archive),
            thousands(c.held_by_http)
        ));
    }
    if !c.overlapped.is_empty() {
        out.push(format!(
            "  overlapped  {}, each met by an earlier fetch's edge, tail or provisional key — not \
             imported; each stays `data hist fetch`'s to fill: {}",
            day_count(c.overlapped.len() as u64),
            date_list(&c.overlapped)
        ));
    }
    if c.too_recent > 0 {
        out.push(format!(
            "  too recent  {} inside the vendor's publication margin — import again later",
            day_count(c.too_recent)
        ));
    }
    if !c.refused.is_empty() {
        out.push(format!(
            "  refused     {} — no commit key is spent for any of them:",
            day_count(c.refused.len() as u64)
        ));
        out.extend(refusal_lines(c.refused.iter().map(|(d, r)| (*d, r))));
    }
    let ticks = match (c.declared_days, c.importable) {
        (0, 0) => String::new(),
        (0, _) => " · tick count unknown (no file header declares one)".to_string(),
        (k, n) if k == n => {
            format!(
                " · {} ticks (exact from {} file headers)",
                thousands(c.declared_ticks),
                thousands(k)
            )
        }
        (k, n) => format!(
            " · {} ticks declared by {} of {} file headers (the rest declare no size)",
            thousands(c.declared_ticks),
            thousands(k),
            thousands(n)
        ),
    };
    let supersede = match c.supersede {
        0 => String::new(),
        s => format!(" ({} of them supersede a provisional fetch window)", thousands(s)),
    };
    out.push(format!("  import      {}{supersede}{ticks}", day_count(c.importable)));
    out.push(if plan.bars.is_empty() {
        format!("  bars        none (--bars {NO_BARS}) — only the ticks are stored")
    } else {
        format!("  bars        {}, resampled per day from the stored ticks", plan.bars.join(", "))
    });
    if !plan.gaps.is_empty() {
        out.push(format!(
            "  gaps        {} with no file — no ticks, per the vendor, or not synced yet: {}",
            counted(plan.gaps.len() as u64, "weekday"),
            date_list(&plan.gaps)
        ));
    }
    out.push(store_line(plan, &c));
    out.push(format!("  price       {}", plan.admission));
    out
}

/// The store-growth line. ⚠ It never GUESSES a size: the bytes per row are MEASURED on the series
/// the store already holds (`ImportPlan::series`, from either lane), and with nothing stored yet the
/// line says the growth is unknown until the first month lands — the design's §5 rule.
fn store_line(plan: &ImportPlan, c: &ClassCounts) -> String {
    let measured =
        plan.series.as_ref().filter(|s| s.rows > 0).map(|s| s.bytes as f64 / s.rows as f64);
    match measured {
        Some(per) if c.declared_ticks > 0 => format!(
            "  store       ≈ +{} ({per:.1} bytes/tick, measured on this series)",
            human_bytes(per * c.declared_ticks as f64)
        ),
        Some(per) => format!(
            "  store       {per:.1} bytes/tick measured on this series — the growth needs a tick \
             count, and no importable file's header declares one"
        ),
        None => "  store       growth unknown until the first month lands — the store holds no \
                 tick of this series to measure bytes per tick from"
            .to_string(),
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

/// What to do when the dataset's directory is not readable on the datahub's box — the design's
/// §2.4 answer: the files must be ON THAT BOX, and there are two ways to put them there, neither of
/// which hands vike a credential.
///
/// For [`DUKASCOPY_FORMAT`] the two lines are paste-ready, with the dataset and the server's own
/// directory already substituted; the AWS account is the operator's own, as the owner's ruling
/// requires.
pub(super) fn dir_lines(plan: &ImportPlan, addr: &str) -> Vec<String> {
    let dir = plan.server_dir.trim_end_matches('/');
    match &plan.dir {
        DatasetDir::Present => Vec::new(),
        DatasetDir::Unreadable { .. } => vec![
            "the directory exists and the datahub's process cannot read it. The daemon's user must \
             be able to READ every file under it — a sync run as you may leave files only you can \
             read — and a unit with ProtectHome=yes cannot read a root symlinked into a home \
             directory. Fix that on the datahub's box, then run this command again."
                .to_string(),
        ],
        DatasetDir::Absent => {
            let mut out = vec![format!(
                "the files must be on the DATAHUB's box — a datahub reached through a tunnel is \
                 127.0.0.1 too, so that path is on whichever box answered at {addr}. Two ways to \
                 put them there, and vike holds no credential for either:"
            )];
            let ds = &plan.dataset;
            if plan.format == DUKASCOPY_FORMAT {
                out.push(
                    "  1. run the sync ON that box, under your own AWS account there:".to_string(),
                );
                out.push(format!(
                    "     aws s3 sync s3://cfg-public-proper-wallaby/{ds}/ {dir}/ --region \
                     eu-west-1 --request-payer requester --profile dukascopy --exclude \"*\" \
                     --include \"*_ticks.bi5\""
                ));
            } else {
                out.push(format!(
                    "  1. download the dataset ON that box, in the vendor's own layout, into {dir}/"
                ));
            }
            out.push(
                "  2. or download it on your PC and copy the folder over the SSH you already use:"
                    .to_string(),
            );
            out.push(format!("     rsync -a ./{ds}/ <box>:{dir}/"));
            out.push("then run this command again.".to_string());
            out
        }
    }
}

/// The sentence the run FAILS on when the directory could not be walked.
pub(super) fn dir_failure(plan: &ImportPlan) -> String {
    match &plan.dir {
        DatasetDir::Absent => format!(
            "{} {}: no such directory on the datahub's box ({}) — nothing was read and nothing was \
             written",
            plan.format, plan.dataset, plan.server_dir
        ),
        _ => format!(
            "{} {}: the datahub cannot read its directory ({}) — nothing was read and nothing was \
             written",
            plan.format, plan.dataset, plan.server_dir
        ),
    }
}

/// The typed confirmation an import asks for — `import N days`, N the plan's
/// `ImportPlan::importable_days`, the number the wire names for exactly this.
///
/// ⚠ Bound to a FACT OF THE PLAN, as `rm`'s `delete N series` is: a line copied from an earlier run
/// against a different plan does not match.
pub(super) fn confirmation(plan: &ImportPlan) -> String {
    format!("import {} days", plan.importable_days())
}

/// How many days of the plan a request of `mode` would ACT on. Zero means there is nothing to send.
///
/// ⚠ **An import counts the HELD days as well as the importable ones**, and dropping them would
/// break the resume this lane promises: a run killed after a day's ticks landed and before its bars
/// did leaves that day HELD, and only an import request derives its missing bars. A verify decodes
/// exactly the importable days, so it counts those alone.
pub(super) fn actionable(plan: &ImportPlan, mode: Mode) -> u64 {
    let c = ClassCounts::of(plan);
    match mode {
        Mode::Plan => 0,
        Mode::Verify => c.importable,
        Mode::Import => c.importable + c.held(),
    }
}

// ─── the month requests ───────────────────────────────────────────────────────────────────────

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

/// How many importable days `plan` counts in `[from, to]`.
fn importable_in(plan: &ImportPlan, from: i64, to: i64) -> u64 {
    plan.days.iter().filter(|d| d.day >= from && d.day <= to && d.class.is_importable()).count()
        as u64
}

/// The line printed before the first month is sent.
pub(super) fn header_line(plan: &ImportPlan, months: &[(i64, i64)], mode: Mode) -> String {
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
pub(super) fn run_months(
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
pub(super) fn summary_lines(preview: &ImportPlan, tally: &Tally, mode: Mode) -> Vec<String> {
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
pub(super) fn refused_failure(refused: usize, mode: Mode) -> String {
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
pub(super) fn refusal(e: String, addr: &str) -> String {
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
pub(super) fn document(
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

#[cfg(test)]
mod tests {
    //! The verb's arithmetic and words, over plans and outcomes built here. The SHIPPED binary
    //! against a real datahub with a planted imports directory is
    //! `crates/vike-cli/tests/data_cli.rs`'s, and the grammar is `crate::cmd::data`'s `data_tests`.

    use vike_datahub_client::archive::{
        ArchiveInventory, BarsWritten, DayOutcome, DayPlan, ImportOutcome,
    };

    use super::*;

    /// Midnight UTC of `y-m-d`, in epoch-ms.
    fn day(y: i64, m: u32, d: u32) -> i64 {
        days_from_civil(y, m, d) * MS_PER_DAY
    }

    fn refusal_of(class: &str) -> DayRefusal {
        DayRefusal { class: class.to_string(), detail: format!("{class} detail") }
    }

    fn plan_day(d: i64, class: DayClass, declared: Option<u64>) -> DayPlan {
        DayPlan { day: d, class, file_bytes: 100, declared_ticks: declared }
    }

    /// A present dataset spanning January–February 2024 with every class in it.
    fn plan(days: Vec<DayPlan>) -> ImportPlan {
        ImportPlan {
            format: DUKASCOPY_FORMAT.to_string(),
            dataset: "EURUSD".to_string(),
            venue: "dukascopy".to_string(),
            server_dir: "/opt/project/market_data/imports/dukascopy-bi5/EURUSD".to_string(),
            dir: DatasetDir::Present,
            admission: "point value 100000 — vendor page, \"most FX pairs\"".to_string(),
            inventory: ArchiveInventory {
                daily_files: days.len() as u64,
                first_day: days.first().map(|d| d.day),
                last_day: days.last().map(|d| d.day),
                other_layout_days: vec![day(2024, 1, 3)],
                other_objects: 2,
                other_bytes: 40,
                skipped: Vec::new(),
            },
            from_day: days.first().map(|d| d.day),
            to_day: days.last().map(|d| d.day),
            days,
            gaps: vec![day(2024, 1, 4)],
            bars: vec!["1m".to_string()],
            series: None,
        }
    }

    // ─── the grammar's two parsers ─────────────────────────────────────────────────────────────

    /// **`--to` is INCLUSIVE**: both bounds parse to the START of the day they name, so
    /// `--from D --to D` is the one day D, whole. A day label, or the epoch-ms of a midnight.
    #[test]
    fn a_day_bound_is_the_start_of_the_day_it_names() {
        assert_eq!(parse_day("--to", "2024-01-15"), Ok(day(2024, 1, 15)));
        assert_eq!(parse_day("--from", "2024-01-15"), Ok(day(2024, 1, 15)));
        assert_eq!(parse_day("--to", &day(2024, 1, 15).to_string()), Ok(day(2024, 1, 15)));
        assert_eq!(parse_day("--to", "1970-01-01"), Ok(0));
    }

    /// An instant inside a day, an hour label, a day that does not exist and nonsense are all
    /// REFUSED rather than rounded or rolled over.
    #[test]
    fn a_bound_that_is_not_one_real_day_is_refused() {
        let inside = (day(2024, 1, 15) + 1).to_string();
        let e = parse_day("--to", &inside).unwrap_err();
        assert!(e.contains("not the START of a UTC day") && e.contains("--to"), "{e}");
        let e = parse_day("--from", "2024-02-30").unwrap_err();
        assert!(e.contains("not a calendar date"), "{e}");
        let e = parse_day("--from", "2024-01-15T10").unwrap_err();
        assert!(e.contains("YYYY-MM-DD"), "{e}");
        assert!(parse_day("--from", "yesterday").is_err());
        assert_eq!(parse_day("--from", "2024-02-29"), Ok(day(2024, 2, 29)), "a real leap day");
    }

    /// `--bars`: absent is `1m`, `none` is no bars, a list is held to the wire's own rules here.
    #[test]
    fn bars_default_to_1m_none_is_empty_and_a_list_meets_the_wires_rules() {
        assert_eq!(parse_bars(None), Ok(vec!["1m".to_string()]));
        assert_eq!(parse_bars(Some("none")), Ok(Vec::<String>::new()));
        assert_eq!(parse_bars(Some("1m,5m, 1h")), Ok(vec!["1m".into(), "5m".into(), "1h".into()]));
        for (bad, needle) in [
            ("7m", "does not divide a UTC day"),
            ("1m,1m", "named twice"),
            ("01m", "canonical"),
            ("1m,5m,15m,1h,4h", "IMPORT_MAX_BAR_INTERVALS"),
            ("none,1m", "stands alone"),
            ("", "EMPTY"),
            ("1m,,5m", "empty entry"),
        ] {
            let e = parse_bars(Some(bad)).unwrap_err();
            assert!(e.contains(needle), "{bad:?} must say {needle:?}: {e}");
        }
    }

    // ─── the month cut ─────────────────────────────────────────────────────────────────────────

    /// **A window over a month boundary is TWO requests**, cut at the 1st, each keeping the
    /// window's own outer bound — and the pieces tile it.
    #[test]
    fn a_window_over_a_month_boundary_is_two_requests() {
        assert_eq!(
            months(day(2024, 1, 30), day(2024, 2, 2)),
            vec![(day(2024, 1, 30), day(2024, 1, 31)), (day(2024, 2, 1), day(2024, 2, 2))]
        );
    }

    /// The cut's other edges: one day is one request; a whole leap February is one; December rolls
    /// into January; and over a long window every piece is inside one month, at most 31 days, and
    /// the pieces tile the window with no day twice and none missing.
    #[test]
    fn the_months_tile_the_window_and_none_is_over_the_wires_day_cap() {
        assert_eq!(
            months(day(2024, 1, 15), day(2024, 1, 15)),
            vec![(day(2024, 1, 15), day(2024, 1, 15))]
        );
        assert_eq!(
            months(day(2024, 2, 1), day(2024, 2, 29)),
            vec![(day(2024, 2, 1), day(2024, 2, 29))]
        );
        assert_eq!(
            months(day(2023, 12, 31), day(2024, 1, 1)),
            vec![(day(2023, 12, 31), day(2023, 12, 31)), (day(2024, 1, 1), day(2024, 1, 1))]
        );
        let (from, to) = (day(2003, 5, 4), day(2026, 9, 27));
        let pieces = months(from, to);
        assert_eq!(pieces.len(), (2026 - 2003) * 12 + (9 - 5) + 1);
        assert_eq!(pieces.first().map(|p| p.0), Some(from));
        assert_eq!(pieces.last().map(|p| p.1), Some(to));
        for pair in pieces.windows(2) {
            assert_eq!(pair[0].1 + MS_PER_DAY, pair[1].0, "a gap or an overlap: {pair:?}");
            assert_eq!(civil_from_days(pair[1].0 / MS_PER_DAY).2, 1, "a cut off the 1st: {pair:?}");
        }
        for (a, b) in &pieces {
            assert_eq!(month_label(*a), month_label(*b), "a piece spans two months");
            let span = (b - a) / MS_PER_DAY + 1;
            assert!((1..=31).contains(&span), "{span} days: over IMPORT_MAX_DAYS");
        }
        assert!(months(day(2024, 2, 1), day(2024, 1, 1)).is_empty(), "an inverted window is none");
    }

    /// The months requested are the server's resolved window clipped to the dataset's own files —
    /// a `--from 1970-01-01` does not cost six hundred empty requests.
    #[test]
    fn the_months_are_clipped_to_the_datasets_own_files() {
        let mut p = plan(vec![
            plan_day(day(2024, 1, 30), DayClass::Free, None),
            plan_day(day(2024, 2, 2), DayClass::Free, None),
        ]);
        p.from_day = Some(0);
        p.to_day = Some(day(2030, 1, 1));
        assert_eq!(
            months_of(&p),
            vec![(day(2024, 1, 30), day(2024, 1, 31)), (day(2024, 2, 1), day(2024, 2, 2))]
        );
        p.inventory.first_day = None;
        p.inventory.last_day = None;
        assert!(months_of(&p).is_empty(), "no daily file: nothing to request");
    }

    /// [`run_months`] sends exactly the cut, in order, and stops at the first failure — naming the
    /// month, what the months before it did, and how to resume.
    #[test]
    fn the_months_are_sent_in_order_and_a_failure_stops_the_run() {
        let p = plan(vec![
            plan_day(day(2024, 1, 30), DayClass::Free, None),
            plan_day(day(2024, 2, 1), DayClass::Free, None),
            plan_day(day(2024, 3, 1), DayClass::Free, None),
        ]);
        let cut = months_of(&p);
        assert_eq!(cut.len(), 3);
        let mut sent = Vec::new();
        let mut lines = Vec::new();
        let answer = |from: i64| ImportDone {
            plan: plan(vec![plan_day(from, DayClass::Free, None)]),
            outcome: Some(ImportOutcome {
                days: vec![DayOutcome {
                    day: from,
                    result: DayResult::Imported { ticks: 5, bars: Vec::new() },
                }],
            }),
        };
        let failed = run_months(
            &cut,
            &p,
            Mode::Import,
            "HEADER",
            |from, to| {
                sent.push((from, to));
                if from == day(2024, 3, 1) {
                    Err("the store failed".to_string())
                } else {
                    Ok(answer(from))
                }
            },
            |line| lines.push(line.to_string()),
        )
        .unwrap_err();
        assert_eq!(sent, cut, "every month up to the failed one, in order");
        assert_eq!(lines.len(), 3, "the header and one line per answered month: {lines:?}");
        assert!(lines[1].starts_with("  1/3  2024-01  1 day  5 ticks  0 refused"), "{lines:?}");
        let msg = failed.message(Mode::Import);
        assert!(msg.starts_with("month 3 of 3 (2024-03) failed"), "{msg}");
        assert!(msg.contains("the 2 months before it imported 2 days, which stay stored"), "{msg}");
        assert!(
            msg.contains("Re-run the same command to resume") && msg.ends_with("the store failed")
        );
    }

    // ─── the plan ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn digits_are_grouped_in_threes() {
        for (n, want) in [(0, "0"), (999, "999"), (1_000, "1,000"), (96_312_004, "96,312,004")] {
            assert_eq!(thousands(n), want);
        }
    }

    /// The plan draws every class the server sent, with its count — and the confirmation names the
    /// IMPORTABLE count, never the held or the refused.
    #[test]
    fn the_plan_names_every_class_and_the_confirmation_names_the_importable_count() {
        let p = plan(vec![
            plan_day(day(2024, 1, 2), DayClass::HeldByArchive, None),
            plan_day(day(2024, 1, 5), DayClass::HeldByHttp, None),
            plan_day(day(2024, 1, 8), DayClass::Free, Some(1_000)),
            plan_day(day(2024, 1, 9), DayClass::Supersede { key: "k".into() }, Some(2_000)),
            plan_day(day(2024, 1, 10), DayClass::Overlapped { keys: vec!["a".into()] }, None),
            plan_day(day(2024, 1, 11), DayClass::Refused(refusal_of("MixedLayout")), None),
            plan_day(day(2024, 1, 12), DayClass::TooRecent, None),
        ]);
        let text = plan_lines(&p, "127.0.0.1:7878").join("\n");
        for needle in [
            "dukascopy-bi5 · EURUSD — datahub at 127.0.0.1:7878",
            "server dir  /opt/project/market_data/imports/dukascopy-bi5/EURUSD   (on the DATAHUB's box)",
            "files       7 daily files 2024-01-02 .. 2024-01-12 · 1 day in another layout",
            "window      2024-01-02 .. 2024-01-12 → 7 daily files",
            "held        2 days (1 by this lane · 1 by the HTTP lane)",
            "overlapped  1 day, each met by an earlier fetch's",
            "2024-01-10",
            "too recent  1 day inside the vendor's publication margin",
            "refused     1 day — no commit key is spent",
            "2024-01-11  MixedLayout — MixedLayout detail",
            "import      2 days (1 of them supersede a provisional fetch window) · 3,000 ticks (exact \
             from 2 file headers)",
            "bars        1m, resampled per day from the stored ticks",
            "gaps        1 weekday with no file",
            "growth unknown until the first month lands",
            "price       point value 100000",
        ] {
            assert!(text.contains(needle), "missing {needle:?}:\n{text}");
        }
        assert_eq!(confirmation(&p), "import 2 days");
        assert_eq!(actionable(&p, Mode::Import), 4, "the importable AND the held: the top-up");
        assert_eq!(actionable(&p, Mode::Verify), 2, "a verify decodes the importable alone");
        assert_eq!(actionable(&p, Mode::Plan), 0);
    }

    /// The store line measures, and never guesses: bytes per tick from the series the store holds,
    /// times the ticks the headers declared.
    #[test]
    fn the_store_growth_is_measured_on_the_series_or_said_to_be_unknown() {
        let mut p = plan(vec![plan_day(day(2024, 1, 8), DayClass::Free, Some(100_000_000))]);
        p.series = Some(vike_data::SeriesCoverage {
            first_ts: 0,
            last_ts: 1,
            rows: 1_000,
            bytes: 12_600,
            parts: 1,
            dates: 1,
        });
        let text = plan_lines(&p, "a:1").join("\n");
        assert!(
            text.contains("store       ≈ +1.3 GB (12.6 bytes/tick, measured on this series)"),
            "{text}"
        );
        p.days[0].declared_ticks = None;
        let text = plan_lines(&p, "a:1").join("\n");
        assert!(text.contains("tick count unknown"), "{text}");
        assert!(text.contains("12.6 bytes/tick measured on this series"), "{text}");
    }

    /// Long lists are capped in the human plan and say how many they withheld.
    #[test]
    fn a_long_list_is_capped_and_says_what_it_withheld() {
        let mut p = plan(vec![plan_day(day(2024, 1, 2), DayClass::Free, None)]);
        p.gaps = (0..25).map(|n| day(2024, 3, 1) + n * MS_PER_DAY).collect();
        let text = plan_lines(&p, "a:1").join("\n");
        assert!(text.contains("25 weekdays with no file"), "{text}");
        assert!(text.contains("… and 15 more (the --json document carries every one)"), "{text}");
        assert!(!text.contains("2024-03-11"), "the eleventh date must be withheld: {text}");
    }

    /// An absent directory names the SERVER's path and the two ways to fill it, paste-ready with
    /// the dataset and the directory substituted; an unreadable one says what to fix; and the plan
    /// stops at the directory line for both.
    #[test]
    fn an_absent_dataset_names_the_servers_path_and_the_two_ways_to_fill_it() {
        let mut p = plan(Vec::new());
        p.dir = DatasetDir::Absent;
        let plan_text = plan_lines(&p, "127.0.0.1:7878");
        assert_eq!(plan_text.len(), 2, "{plan_text:?}");
        assert!(plan_text[1].contains("NOT FOUND on the datahub's box"), "{plan_text:?}");
        let text = dir_lines(&p, "127.0.0.1:7878").join("\n");
        for needle in [
            "the files must be on the DATAHUB's box",
            "answered at 127.0.0.1:7878",
            "aws s3 sync s3://cfg-public-proper-wallaby/EURUSD/ \
             /opt/project/market_data/imports/dukascopy-bi5/EURUSD/ --region eu-west-1 \
             --request-payer requester --profile dukascopy --exclude \"*\" --include \"*_ticks.bi5\"",
            "rsync -a ./EURUSD/ <box>:/opt/project/market_data/imports/dukascopy-bi5/EURUSD/",
            "then run this command again",
        ] {
            assert!(text.contains(needle), "missing {needle:?}:\n{text}");
        }
        assert!(!text.contains("--delete"), "a sync line must never carry --delete: {text}");
        assert!(dir_failure(&p).contains("no such directory on the datahub's box"));

        p.format = "another-format".to_string();
        let text = dir_lines(&p, "a:1").join("\n");
        assert!(!text.contains("aws s3"), "no vendor line for a format with no hint: {text}");
        assert!(text.contains("rsync -a ./EURUSD/"), "{text}");

        p.dir = DatasetDir::Unreadable { why: "PermissionDenied".to_string() };
        assert!(
            plan_lines(&p, "a:1")[1]
                .contains("EXISTS, and the datahub cannot read it: PermissionDenied")
        );
        assert!(dir_lines(&p, "a:1")[0].contains("ProtectHome=yes"));
        assert!(dir_failure(&p).contains("cannot read its directory"));
    }

    /// The one format id this side keys a hint on is the SERVER's registry id, read from that
    /// crate's source rather than restated — this crate's test build cannot link the registry,
    /// which lives behind `backfill-serve`.
    #[test]
    fn the_sync_hint_names_the_servers_own_format_id() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../vike-datahub/src/import/formats.rs");
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert!(
            src.contains(&format!("pub const DUKASCOPY_BI5: &str = \"{DUKASCOPY_FORMAT}\";")),
            "the server's registry id moved, and the sync hint would never be printed"
        );
    }

    // ─── the months' outcome ───────────────────────────────────────────────────────────────────

    fn month(from: i64, days: Vec<DayPlan>, rows: Vec<DayOutcome>, preview: u64) -> MonthDone {
        MonthDone {
            from,
            to: from,
            done: ImportDone { plan: plan(days), outcome: Some(ImportOutcome { days: rows }) },
            preview_importable: preview,
            elapsed: Duration::from_millis(1_500),
        }
    }

    /// A refused day is a DEFECT only when the server's plan did not already name it too recent,
    /// overlapped or refused: an overlapped day's echo in the outcome never fails a run, a
    /// plan-time refusal is counted once, and a day the plan called importable that the decode
    /// refused is a defect.
    #[test]
    fn only_a_refusal_for_what_is_in_the_file_is_a_defect() {
        let (d1, d2, d3, d4) =
            (day(2024, 1, 8), day(2024, 1, 9), day(2024, 1, 10), day(2024, 1, 11));
        let m = month(
            d1,
            vec![
                plan_day(d1, DayClass::Free, Some(4)),
                plan_day(d2, DayClass::Free, Some(4)),
                plan_day(d3, DayClass::Overlapped { keys: vec!["k".into()] }, None),
                plan_day(d4, DayClass::Refused(refusal_of("MixedLayout")), None),
            ],
            vec![
                DayOutcome {
                    day: d1,
                    result: DayResult::Imported {
                        ticks: 4,
                        bars: vec![BarsWritten { interval: "1m".into(), rows: 3 }],
                    },
                },
                DayOutcome { day: d2, result: DayResult::Refused(refusal_of("AmbiguousTimeBase")) },
                DayOutcome { day: d3, result: DayResult::Refused(refusal_of("Overlapped")) },
                DayOutcome { day: d4, result: DayResult::Refused(refusal_of("MixedLayout")) },
            ],
            2,
        );
        let t = Tally::of_month(&m.done);
        assert_eq!((t.imported, t.ticks, t.bars, t.overlapped), (1, 4, 3, 1));
        let classes: Vec<&str> = t.refused.iter().map(|(_, r)| r.class.as_str()).collect();
        assert_eq!(classes, vec!["AmbiguousTimeBase", "MixedLayout"], "{t:?}");
        let line = progress_line(1, 1, &m, Mode::Import);
        assert!(line.starts_with("  1/1  2024-01  1 day  4 ticks  2 refused  1.5s"), "{line}");
        assert!(
            line.ends_with("(2024-01-09: AmbiguousTimeBase — see --dry-run --verify)"),
            "{line}"
        );
        assert!(is_a_defect(None), "a day the plan never named is a defect if refused");
        assert!(is_a_defect(Some(&DayClass::HeldByArchive)), "a failed top-up is a defect");
    }

    /// **Two plans, one authority**: when the datahub's month plans count a different number of
    /// importable days than the preview, the summary prints BOTH and names the datahub's as the
    /// authority — and the month whose plan moved says so on its own line. Equal counts print
    /// nothing about it.
    #[test]
    fn when_the_two_plans_differ_both_counts_are_printed() {
        let (d1, d2) = (day(2024, 1, 8), day(2024, 1, 9));
        let preview =
            plan(vec![plan_day(d1, DayClass::Free, None), plan_day(d2, DayClass::Free, None)]);
        let moved = month(
            d1,
            vec![plan_day(d1, DayClass::Free, None), plan_day(d2, DayClass::HeldByHttp, None)],
            vec![
                DayOutcome { day: d1, result: DayResult::Imported { ticks: 1, bars: Vec::new() } },
                DayOutcome { day: d2, result: DayResult::ToppedUp { bars: Vec::new() } },
            ],
            2,
        );
        let t = Tally::of(std::slice::from_ref(&moved));
        let line = plans_differ_line(&preview, &t).expect("the counts differ");
        assert!(line.contains("the preview counted 2 days importable"), "{line}");
        assert!(line.contains("the datahub counted 1 day as it went"), "{line}");
        assert!(line.contains("The datahub's count is the authority"), "{line}");
        let summary = summary_lines(&preview, &t, Mode::Import).join("\n");
        assert!(summary.contains(&line), "{summary}");
        assert!(summary.ends_with(
            "done — 1 day imported (1 ticks, 0 bars) · 0 refused · 0 overlapped · 1 gap · 1 already held"
        ), "{summary}");
        assert!(
            progress_line(1, 1, &moved, Mode::Import)
                .contains("(the datahub planned 1 importable here, the preview 2)"),
        );

        let same = plan(vec![
            plan_day(d1, DayClass::Free, None),
            plan_day(d2, DayClass::HeldByHttp, None),
        ]);
        assert_eq!(plans_differ_line(&same, &t), None, "equal counts say nothing");
    }

    /// A verify's summary counts decoded days and says nothing was written.
    #[test]
    fn a_verify_summary_counts_decoded_days_and_says_nothing_was_written() {
        let d1 = day(2024, 1, 8);
        let m = month(
            d1,
            vec![plan_day(d1, DayClass::Free, None)],
            vec![DayOutcome { day: d1, result: DayResult::Verified { ticks: 1_234 } }],
            1,
        );
        let t = Tally::of(std::slice::from_ref(&m));
        let preview = plan(vec![plan_day(d1, DayClass::Free, None)]);
        let summary = summary_lines(&preview, &t, Mode::Verify);
        assert_eq!(
            summary.last().map(String::as_str),
            Some("verified — 1 day decoded clean (1,234 ticks) · 0 refused · nothing was written")
        );
        assert!(
            progress_line(1, 1, &m, Mode::Verify).starts_with("  1/1  2024-01  1 day  1,234 ticks")
        );
    }

    /// The `--json` document carries the preview in the wire's own words, one object per month
    /// with the server's outcome, both importable counts and the defect days — and a plan-only run
    /// has no months and no summary.
    #[test]
    fn the_document_carries_the_plan_the_months_and_both_counts() {
        let d1 = day(2024, 1, 8);
        let preview = plan(vec![plan_day(d1, DayClass::Free, Some(9))]);
        let m = month(
            d1,
            vec![plan_day(d1, DayClass::Free, Some(9))],
            vec![DayOutcome { day: d1, result: DayResult::Refused(refusal_of("NotMonotonic")) }],
            1,
        );
        let doc = document("127.0.0.1:7878", Mode::Import, &preview, std::slice::from_ref(&m));
        assert_eq!(doc["mode"], "import");
        assert_eq!(doc["confirmation"], "import 1 days");
        assert_eq!(doc["plan"]["server_dir"], preview.server_dir.as_str());
        assert_eq!(doc["plan"]["days"][0]["class"], "Free");
        assert_eq!(doc["months"][0]["month"], "2024-01");
        assert_eq!(
            doc["months"][0]["outcome"]["days"][0]["result"]["Refused"]["class"],
            "NotMonotonic"
        );
        assert_eq!(doc["summary"]["preview_importable"], 1);
        assert_eq!(doc["summary"]["server_importable"], 1);
        assert_eq!(doc["summary"]["refused"][0]["date"], "2024-01-08");

        let plan_only = document("a:1", Mode::Plan, &preview, &[]);
        assert_eq!(plan_only["months"], serde_json::json!([]));
        assert_eq!(plan_only["summary"], serde_json::Value::Null);
        assert_eq!(plan_only["confirmation"], serde_json::Value::Null);
    }

    /// The capability refusal gains what to DO; every other refusal is the server's own sentence.
    #[test]
    fn the_capability_refusal_says_what_to_do_and_nothing_else_is_rewritten() {
        let missing = "datahub server does not advertise `archive_import` (advertised: []) — nothing \
                       was sent.";
        let e = refusal(missing.to_string(), "127.0.0.1:7878");
        assert!(e.starts_with(missing), "{e}");
        assert!(e.contains("the datahub at 127.0.0.1:7878 has no archive import lane"), "{e}");
        assert!(e.contains("v0.1.41") && e.contains("ARCHIVE IMPORT lane mounted"), "{e}");
        let other = "ImportArchive: another archive import (or `verify`) is already running";
        assert_eq!(refusal(other.to_string(), "a:1"), other);
    }

    /// Every `vike-cli …` line this verb's answers tell an operator to type PARSES — held to the
    /// real grammar through `crate::cmd::accepts`, never to a second copy of its spelling.
    #[test]
    fn every_command_these_answers_tell_an_operator_to_type_parses() {
        let backticked = |line: &str| -> Vec<String> {
            line.split('`').skip(1).step_by(2).map(String::from).collect()
        };
        let p = plan(vec![plan_day(day(2024, 1, 8), DayClass::Free, None)]);
        let mut texts: Vec<String> = plan_lines(&p, "a:1");
        texts.extend(dir_lines(
            &{
                let mut a = p.clone();
                a.dir = DatasetDir::Absent;
                a
            },
            "a:1",
        ));
        texts.push(refused_failure(1, Mode::Import));
        texts.push(refusal(format!("does not advertise `{FEATURE_ARCHIVE_IMPORT}`"), "a:1"));
        let commands: Vec<String> = texts
            .iter()
            .flat_map(|t| backticked(t))
            .filter(|c| c.starts_with("vike-cli "))
            .collect();
        for command in &commands {
            let argv: Vec<&str> = command.split_whitespace().collect();
            crate::cmd::accepts(&argv[1..]).unwrap_or_else(|e| {
                panic!("this answer tells an operator to run `{command}`: {e}")
            });
        }
        // The pointer the progress line prints is a FLAG pair, not a whole command: it must name
        // flags the verb takes together.
        crate::cmd::accepts(&[
            "data",
            "hist",
            "import",
            DUKASCOPY_FORMAT,
            "EURUSD",
            "--dry-run",
            "--verify",
        ])
        .expect("`--dry-run --verify` is a line the import verb accepts");
    }
}
