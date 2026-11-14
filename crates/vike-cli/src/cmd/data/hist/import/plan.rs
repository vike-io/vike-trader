//! The PLAN: the month cut and every line the preview prints. The flow is in `super`'s module doc.
use vike_datahub_client::archive::{ImportDatasetDirectory, ImportPlan};
use vike_model::MS_PER_DAY;
use vike_model::time::{epoch_ms_to_utc_date, epoch_ms_to_utc_timestamp};
use vike_utility::bytes::iec_bytes;
use vike_utility::calendar::{civil_from_days, days_from_civil};

use super::{
    CSV_FORMAT, ClassCounts, DUKASCOPY_FORMAT, KLINES_FORMAT, MAX_LISTED, Mode, NO_BARS, counted,
    day_count, day_lines, refusal_lines, thousands,
};

// ─── the month cut ────────────────────────────────────────────────────────────────────────────

/// The calendar months of the inclusive day window `[from, to]` (both UTC midnights), as inclusive
/// `(first day, last day)` pairs — the first and last keeping the window's own bounds.
///
/// ⚠ **The pieces TILE the window**: consecutive pieces are one day apart, so no day is in two
/// requests and none in neither, and every interior cut is a 1st of the month. A request is then at
/// most 31 days, inside the wire's `IMPORT_MAX_DAYS`, which is the whole reason a month is the unit.
pub(crate) fn months(from: i64, to: i64) -> Vec<(i64, i64)> {
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
pub(crate) fn months_of(plan: &ImportPlan) -> Vec<(i64, i64)> {
    let inventory = &plan.inventory;
    match (plan.from_day, plan.to_day, inventory.first_day, inventory.last_day) {
        (Some(from), Some(to), Some(first), Some(last)) if from.max(first) <= to.min(last) => {
            months(from.max(first), to.min(last))
        }
        _ => Vec::new(),
    }
}

// ─── the plan ─────────────────────────────────────────────────────────────────────────────────

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

/// The plan as the operator reads it — the design's §7.2 drawing, every line a fact the SERVER
/// sent. The first line names the datahub and the second the directory ON ITS BOX, because a
/// tunnelled datahub is `127.0.0.1` too and that path is how an operator learns which box the files
/// must be on.
///
/// For an absent or unreadable directory it stops after the directory line; [`dir_lines`] says
/// what to do about it.
pub(crate) fn plan_lines(plan: &ImportPlan, addr: &str) -> Vec<String> {
    let mut out = vec![format!("{} · {} — datahub at {addr}", plan.format, plan.dataset)];
    match &plan.dir {
        ImportDatasetDirectory::Present => {
            out.push(format!("  server dir  {}   (on the DATAHUB's box)", plan.server_dir));
        }
        ImportDatasetDirectory::Absent => {
            out.push(format!("  server dir  {}   NOT FOUND on the datahub's box", plan.server_dir));
            return out;
        }
        ImportDatasetDirectory::Unreadable { why } => {
            out.push(format!(
                "  server dir  {}   EXISTS, and the datahub cannot read it: {why}",
                plan.server_dir
            ));
            return out;
        }
    }
    let inventory = &plan.inventory;
    let span = match (inventory.first_day, inventory.last_day) {
        (Some(a), Some(b)) => {
            format!(" {} .. {}", epoch_ms_to_utc_date(a), epoch_ms_to_utc_date(b))
        }
        _ => String::new(),
    };
    out.push(format!(
        "  files       {} daily files{span} · {} in another layout (counted, not imported) · {} \
         other objects (not read)",
        thousands(inventory.daily_files),
        day_count(inventory.other_layout_days.len() as u64),
        thousands(inventory.other_objects)
    ));
    if let Some(first) = inventory.skipped.first() {
        out.push(format!(
            "  skipped     {} entries the walk would not follow or open (a symlink, a FIFO, a hard \
             link …) — the first is {} ({:?})",
            inventory.skipped.len(),
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
    let dukascopy = plan.format == DUKASCOPY_FORMAT;
    if c.held() > 0 {
        out.push(format!(
            "  held        {} ({} by this lane · {} by {}) — not read again; their bars are topped \
             up if missing",
            day_count(c.held()),
            thousands(c.held_by_archive),
            thousands(c.held_by_http),
            if dukascopy { "the HTTP lane" } else { "another lane" }
        ));
    }
    if !c.overlapped.is_empty() {
        let why = if dukascopy {
            "each met by an earlier fetch's edge, tail or provisional key — not imported; each stays \
             `data hist fetch`'s to fill"
        } else {
            "each already holding rows another writer stored (the live recorder, or a key this \
             format cannot read; --json names them) — not imported, so no row is stored twice"
        };
        out.push(format!(
            "  overlapped  {}, {why}: {}",
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
    if !c.replace.is_empty() {
        out.push(format!(
            "  replace     {} (--replace-holed), each held by another producer with fewer bars \
             than a whole day — replaced whole by the archive's verified day:",
            day_count(c.replace.len() as u64)
        ));
        out.extend(day_lines(c.replace.iter().map(|(day, stored, full)| {
            (*day, format!("{} of {} bars stored", thousands(*stored), thousands(*full)))
        })));
    }
    out.push(if plan.format == KLINES_FORMAT {
        format!("  bars        {}, the vendor's own candles as published", plan.bars.join(", "))
    } else if plan.format == CSV_FORMAT {
        "  bars        the file's own, stored only in its user lane".to_string()
    } else if plan.bars.is_empty() {
        format!("  bars        none (--bars {NO_BARS}) — only the ticks are stored")
    } else {
        format!("  bars        {}, resampled per day from the stored ticks", plan.bars.join(", "))
    });
    if !plan.gaps.is_empty() {
        out.push(format!(
            "  gaps        {} with no file — {}, or not synced yet: {}",
            counted(plan.gaps.len() as u64, "weekday"),
            if dukascopy { "no ticks, per the vendor" } else { "not published" },
            date_list(&plan.gaps)
        ));
    }
    out.push(store_line(plan, &c));
    out.push(format!("  price       {}", plan.admission));
    out.extend(preview_lines(plan));
    out
}

/// The first rows of the dataset as the datahub's decoder READ them (`ImportPlan::preview`, a
/// `csv-bars` plan's): the UTC time and the five numbers, so a user sees how the file was understood
/// before anything is written. Nothing for a plan that carries none.
pub(crate) fn preview_lines(plan: &ImportPlan) -> Vec<String> {
    if plan.preview.is_empty() {
        return Vec::new();
    }
    let mut out = vec![format!(
        "  read as     the first {} as the datahub's decoder read them (time in UTC):",
        counted(plan.preview.len() as u64, "row")
    )];
    out.extend(plan.preview.iter().map(|b| {
        format!(
            "              {}  open {}  high {}  low {}  close {}  volume {}",
            epoch_ms_to_utc_timestamp(b.timestamp),
            b.open,
            b.high,
            b.low,
            b.close,
            b.volume
        )
    }));
    out
}

/// The store-growth line. ⚠ It never GUESSES a size: the bytes per row are MEASURED on the series
/// the store already holds (`ImportPlan::series`, from either lane), and with nothing stored yet the
/// line says the growth is unknown until the first month lands — the design's §5 rule.
fn store_line(plan: &ImportPlan, c: &ClassCounts) -> String {
    let measured =
        plan.series.as_ref().filter(|s| s.rows > 0).map(|s| s.bytes as f64 / s.rows as f64);
    match measured {
        // The estimate is fractional (a measured bytes-per-tick times a tick count); `iec_bytes`
        // takes whole bytes, so it is rounded to the nearest byte here.
        Some(per) if c.declared_ticks > 0 => format!(
            "  store       ≈ +{} ({per:.1} bytes/tick, measured on this series)",
            iec_bytes((per * c.declared_ticks as f64).round() as u64)
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

/// What to do when the dataset's directory is not readable on the datahub's box — the design's
/// §2.4 answer: the files must be ON THAT BOX, and there are two ways to put them there.
///
/// For [`DUKASCOPY_FORMAT`] the first is the datahub's own download (`--fetch`, the owner's ruling
/// of 2026-11-13, which replaced the `aws s3 sync` an operator ran by hand), with the AWS key the
/// operator stored on that box; the lines are paste-ready, the dataset and the server's own
/// directory substituted. Every other format's files are put there by hand.
pub(crate) fn dir_lines(plan: &ImportPlan, addr: &str) -> Vec<String> {
    let dir = plan.server_dir.trim_end_matches('/');
    match &plan.dir {
        ImportDatasetDirectory::Present => Vec::new(),
        ImportDatasetDirectory::Unreadable { .. } => vec![
            "the directory exists and the datahub's process cannot read it. The daemon's user must \
             be able to READ every file under it — a sync run as you may leave files only you can \
             read — and a unit with ProtectHome=yes cannot read a root symlinked into a home \
             directory. Fix that on the datahub's box, then run this command again."
                .to_string(),
        ],
        ImportDatasetDirectory::Absent => {
            let mut out = vec![format!(
                "the files must be on the DATAHUB's box — a datahub reached through a tunnel is \
                 127.0.0.1 too, so that path is on whichever box answered at {addr}. Two ways to \
                 put them there:"
            )];
            let dataset = &plan.dataset;
            if plan.format == DUKASCOPY_FORMAT {
                out.push(
                    "  1. let that datahub download them from Dukascopy's archive, with the AWS key \
                     stored on its box (the plan says what to store when it holds none, and what \
                     the download costs):"
                        .to_string(),
                );
                out.push(format!(
                    "     vike-cli data hist import {DUKASCOPY_FORMAT} {dataset} --from YYYY-MM-DD --to \
                     YYYY-MM-DD --fetch --dry-run"
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
            out.push(format!("     rsync -a ./{dataset}/ <box>:{dir}/"));
            out.push("then run this command again.".to_string());
            out
        }
    }
}

/// The sentence the run FAILS on when the directory could not be walked.
pub(crate) fn dir_failure(plan: &ImportPlan) -> String {
    match &plan.dir {
        ImportDatasetDirectory::Absent => format!(
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
pub(crate) fn confirmation(plan: &ImportPlan) -> String {
    format!("import {} days", plan.importable_days())
}

/// How many days of the plan a request of `mode` would ACT on. Zero means there is nothing to send.
///
/// ⚠ **An import counts the HELD days as well as the importable ones**, and dropping them would
/// break the resume this lane promises: a run killed after a day's ticks landed and before its bars
/// did leaves that day HELD, and only an import request derives its missing bars. A verify decodes
/// exactly the importable days, so it counts those alone.
pub(crate) fn actionable(plan: &ImportPlan, mode: Mode) -> u64 {
    let c = ClassCounts::of(plan);
    match mode {
        Mode::Plan => 0,
        Mode::Verify => c.importable,
        Mode::Import => c.importable + c.held(),
    }
}
