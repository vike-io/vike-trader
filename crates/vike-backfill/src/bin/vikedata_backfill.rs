//! `data.vike.io` cohort-metrics backfill CLI — the graded positioning panel (`kind=cohort`) for one
//! asset's ladder, into the DataFusion hist store. The sibling of `tardis_backfill` /
//! `databento_backfill`, and the reason `crates/vike-backfill/src/vikedata/` exists as a directory:
//! that module's doc carries the argument.
//!
//! THREE run shapes, and the one a TIMER may use is the third:
//!
//!   # SCHEDULED — the only shape a timer may run. No boundary is passed, so no overlapping
//!   # boundary CAN be passed: the windows are whole COMPLETE UTC days off a fixed grid and the
//!   # ladders come from the declared polled set
//!   # (`crates/vike-backfill/src/vikedata/schedule.rs`'s `POLLED`).
//!   vikedata_backfill --asset BTC --schedule --catch-up 3
//!
//!   # periodic — catch up to now. ⚠ MANUAL ONLY: a trailing window SLIDES with the clock, so two
//!   # firings are two commit keys over overlapping hours and the shared hours land TWICE.
//!   vikedata_backfill --asset BTC --axis size --days 30
//!
//!   # AD-HOC — fill a measured gap with ONE command, exact bounds, nothing sliding
//!   vikedata_backfill --asset BTC --axis pnl --grading unrealized \
//!       --start 2026-04-01 --end 2026-04-08
//!
//! Credentials: `VIKE_API_KEY` from the credential store (the settings database) — the SAME key
//! `crates/vike-research/src/bin/research.rs` read for this vendor, and since that binary
//! was deleted with the research crate, THIS bin is now the only reader of it (its registry row is
//! `crates/vike-config/src/settings/rows/research.rs`'s `VIKE_API_KEY`/`vike-backfill`; the dead citation is filed
//! in `crates/vike-ops/tests/docs/citation_gate/dead_paths.rs`'s `DEAD_PATH_EXCEPTIONS`). ABSENT IS A REFUSAL,
//! not a fallback: an unauthenticated fetch 403s part-way through a window and would store a
//! partial batch under a commit key claiming the whole one, which is indistinguishable from a thin
//! tape afterwards.
//!
//! ⚠ Everything environmental is read HERE, out of ONE `std::env::vars()` sweep, and threaded down
//! as parameters — `crates/vike-backfill/src/vikedata/client.rs` and its siblings read no
//! environment and open no credential store.

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use vike_backfill::cli::{log_config, parse, scoped_credentials, store_root};
use vike_backfill::vikedata::{
    Axis, CohortWindow, DEFAULT_CATCH_UP, Grading, Ladder, MAX_CATCH_UP, POLLED, VIKEDATA_BASE,
    backfill_cohort, fetch_panel, panel_commit_key, scheduled_windows,
};
use vike_data::CohortRecorder;
use vike_secrets::KeyScope;
use vike_store::DataFusionHist;

/// The cohort API credential, looked up in the CREDENTIAL STORE's map — never the process env.
/// A `const` rather than a bare literal because `crates/vike-model/src/scan.rs` resolves the
/// indirection, and the constant is the doc anchor the settings-registry row cites.
const API_KEY_ENV: &str = "VIKE_API_KEY";

/// **Every credential name this binary declares** — owner ruling 2026-09-16, a process
/// materialises only what it asked for.
///
/// One name, and it is [`API_KEY_ENV`] itself rather than a second spelling of it: the scope and
/// the lookup then cannot drift into disagreement by an edit to either one alone.
const SCOPE: [&str; 1] = [API_KEY_ENV];

/// The cohort API key from the SCOPED credential read — one name materialised, not the store. The
/// read itself is `vike_backfill::cli::scoped_credentials`, made by each caller out of the one
/// `std::env::vars()` sweep it already owns; this is the lookup, split out so the SCOPE can be
/// exercised against the real lookup without a store on disk.
///
/// # ⚠ `Ok(None)` and `Err` are different events and this bin acts differently on them
///
/// `Ok(None)` is the credential genuinely not being in the store — the live gate, and this bin's
/// documented refusal. `Err` is [`API_KEY_ENV`] having fallen out of [`SCOPE`], which is a defect
/// in this binary rather than a fact about the box; an `Option` return would have made the two
/// indistinguishable, which is the failure `vike_secrets::Lookup` exists to prevent.
///
/// # Errors
/// [`vike_secrets::UndeclaredKey`] when the name asked for is outside [`SCOPE`].
fn api_key(
    store: &vike_secrets::ScopedSecrets,
) -> Result<Option<String>, vike_secrets::UndeclaredKey> {
    Ok(store.get(API_KEY_ENV).declared()?.map(str::to_string))
}

/// The cohort API root: `--api-base URL`, else [`VIKEDATA_BASE`]. (A blank `--api-base` never gets
/// here: every blank value is refused when the command line is read.)
fn api_base(raw: Option<&str>) -> String {
    raw.map_or_else(|| VIKEDATA_BASE.to_string(), str::to_string)
}

/// The default exchange whose positions are graded — and therefore the store `venue`.
const DEFAULT_EXCHANGE: &str = "hyperliquid";

/// A window boundary: `YYYY-MM-DD`, `YYYY-MM-DDTHH`, or bare unix SECONDS.
///
/// Seconds rather than the milliseconds `vike_model::parse_date_label` returns, because this
/// vendor's whole vocabulary is whole seconds on the hour and the seconds→milliseconds conversion
/// belongs in ONE place (`crates/vike-backfill/src/vikedata/parse.rs`'s `hour_ms`), at the row
/// boundary, where it is guarded. A CLI that also multiplied would be the second place.
fn boundary_secs(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(n) = s.parse::<i64>() {
        return Some(n);
    }
    if let Some((y, m, d, h)) = vike_model::time::parse_hour_label(s) {
        return Some(vike_util::calendar::days_from_civil(y, m, d) * 86_400 + i64::from(h) * 3_600);
    }
    let (y, m, d) = vike_model::parse_ymd(s).ok()?;
    Some(vike_util::calendar::days_from_civil(y, m, d) * 86_400)
}

/// Resolve the two run shapes from argv. `--days` is the periodic one; `--start`/`--end` the ad-hoc
/// one. Mixing them is a usage error rather than a silent precedence: an operator who typed both
/// has a window in mind, and guessing which would fill the wrong one.
fn resolve_window(
    days: Option<&str>,
    start: Option<&str>,
    end: Option<&str>,
    now_secs: i64,
) -> Result<CohortWindow, String> {
    match (days, start, end) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            Err("--days and --start/--end are two different run shapes; pass one".to_string())
        }
        (Some(d), None, None) => {
            let n: u32 = d.parse().map_err(|_| format!("--days '{d}' is not a whole number"))?;
            if n == 0 {
                return Err("--days must be at least 1".to_string());
            }
            Ok(CohortWindow::trailing(n, now_secs))
        }
        (None, Some(s), Some(e)) => {
            let s = boundary_secs(s).ok_or_else(|| format!("--start '{s}' is not a date"))?;
            let e = boundary_secs(e).ok_or_else(|| format!("--end '{e}' is not a date"))?;
            CohortWindow::range(s, e).map_err(|err| err.to_string())
        }
        (None, Some(_), None) => Err("--start needs --end".to_string()),
        (None, None, Some(_)) => Err("--end needs --start".to_string()),
        (None, None, None) => {
            Err("pass --schedule, or --days N, or --start DATE --end DATE".to_string())
        }
    }
}

/// The window/ladder flags, as they came off argv. A struct rather than seven positional arguments
/// because [`resolve_plan`]'s whole job is judging COMBINATIONS of them, and a caller that swapped
/// two `Option<&str>`s would still compile.
#[derive(Default)]
struct WindowArgs<'a> {
    schedule: bool,
    catch_up: Option<&'a str>,
    days: Option<&'a str>,
    start: Option<&'a str>,
    end: Option<&'a str>,
    axis: Option<&'a str>,
    grading: Option<&'a str>,
}

/// What one invocation will actually fetch.
#[derive(Debug, PartialEq, Eq)]
enum RunPlan {
    /// One operator-chosen ladder over one operator-chosen window — the two manual shapes.
    AdHoc { axis: Axis, grading: Grading, window: CohortWindow },
    /// The declared polled set over the declared day grid. The operator chose neither.
    Scheduled { windows: Vec<CohortWindow> },
}

/// ⚠ **The refusal below is the schedule's safety, not a tidiness rule.**
///
/// This store's idempotency is BATCH-level on the commit key, so two OVERLAPPING windows are two
/// keys and every hour they share lands TWICE
/// (`crates/vike-backfill/src/vikedata/ingest_tests.rs`'s
/// `two_overlapping_windows_are_two_batches_and_the_shared_hours_land_twice`). `--schedule` derives
/// its windows from a fixed UTC-day grid and therefore cannot express an overlapping one — but only
/// while no boundary flag can reach it. `--schedule --days 30` would be exactly the sliding trailing
/// window the grid exists to replace, wearing the safe shape's name, so it is a usage error rather
/// than a precedence. Same for `--axis`/`--grading`: the polled set is a REVIEWED table with a
/// written reason on every ladder it leaves out, and a unit file that retyped one would be a second
/// copy of that decision, in the place least likely to be revisited.
fn resolve_plan(a: &WindowArgs<'_>, now_secs: i64) -> Result<RunPlan, String> {
    if !a.schedule {
        if a.catch_up.is_some() {
            return Err("--catch-up is the SCHEDULED shape's look-back; it needs --schedule. A \
                        manual run names its own window with --days or --start/--end."
                .to_string());
        }
        let axis_raw = a.axis.unwrap_or("size");
        let axis = Axis::parse(axis_raw)
            .ok_or_else(|| format!("unknown --axis '{axis_raw}': size|pnl|tier"))?;
        let grading_raw = a.grading.unwrap_or("realized");
        let grading = Grading::parse(grading_raw).ok_or_else(|| {
            format!("unknown --grading '{grading_raw}': realized|realized-pit|unrealized")
        })?;
        let window = resolve_window(a.days, a.start, a.end, now_secs)?;
        return Ok(RunPlan::AdHoc { axis, grading, window });
    }

    for (flag, present) in
        [("--days", a.days.is_some()), ("--start", a.start.is_some()), ("--end", a.end.is_some())]
    {
        if present {
            return Err(format!(
                "--schedule and {flag} are two different run shapes; pass one. --schedule derives \
                 whole COMPLETE UTC days off a fixed grid precisely so no boundary is typed — a \
                 typed one slides with the clock, and two firings over overlapping hours are two \
                 commit keys, so the shared hours land TWICE."
            ));
        }
    }
    for (flag, present) in [("--axis", a.axis.is_some()), ("--grading", a.grading.is_some())] {
        if present {
            return Err(format!(
                "--schedule polls the DECLARED ladder set \
                 (crates/vike-backfill/src/vikedata/schedule.rs's POLLED), so {flag} has no meaning \
                 here: a schedule that retyped it would be a second copy of a decision that carries \
                 a written reason for every ladder it leaves out. Use --axis/--grading with \
                 --start/--end for an ad-hoc read."
            ));
        }
    }

    let catch_up = match a.catch_up {
        None => DEFAULT_CATCH_UP,
        Some(v) => v.parse::<u32>().map_err(|_| {
            format!("--catch-up '{v}' is not a whole number (1..={MAX_CATCH_UP} days of look-back)")
        })?,
    };
    let windows = scheduled_windows(now_secs, catch_up).map_err(|e| e.to_string())?;
    Ok(RunPlan::Scheduled { windows })
}

/// Fetch data.vike.io cohort metrics for one asset and ingest them as `kind=cohort` rows.
///
/// Idempotent per WINDOW by the `cohort:{venue}:{asset}:{axis}:{grading}:{label_basis}:{start}-{end}`
/// commit key: re-running the SAME command writes nothing. Two OVERLAPPING windows are two keys, so
/// their shared hours land TWICE. --schedule is the shape that cannot express one.
///
/// Three run shapes. SCHEDULED (--schedule [--catch-up N]) is the only shape a timer may run: no
/// boundary is typed, so no overlapping boundary can be. MANUAL (--days N, or --start/--end, with
/// optional --axis/--grading) is an operator at a prompt; --days SLIDES with the clock, so never put
/// it on a timer. PANEL (--panel --start D --end D) fetches the hourly asset panel.
#[derive(clap::Parser, Debug)]
#[command(
    after_help = "credential store:\n  VIKE_API_KEY     REQUIRED — absent is a refusal, never a \
                        fallback"
)]
struct Args {
    /// The coin whose positions were graded, e.g. BTC (the series `symbol`)
    #[arg(long)]
    asset: String,
    /// The venue whose positions were graded, and the series `venue`
    #[arg(long, default_value = DEFAULT_EXCHANGE)]
    exchange: String,
    /// Hist-store root (default: <repo>/market_data/hist, else the project's own)
    #[arg(long)]
    store: Option<String>,
    /// API root (default: https://data.vike.io/v1)
    #[arg(long)]
    api_base: Option<String>,

    /// SCHEDULED: fetch whole COMPLETE UTC days off a fixed grid, over the DECLARED polled ladders
    /// (crates/vike-backfill/src/vikedata/schedule.rs's POLLED). Two firings inside one UTC day ask
    /// for the identical windows, so the cadence cannot double a row; two days are adjacent, so it
    /// cannot leave a gap either. The still-filling day is never fetched. Refuses
    /// --days/--start/--end/--axis/--grading.
    #[arg(long)]
    schedule: bool,
    /// SCHEDULED: also re-attempt the N-1 preceding days, so a firing missed to an outage heals
    /// itself (default 1, max 31). Every extra day is a real metered request on EVERY firing whether
    /// or not it writes a row.
    #[arg(long)]
    catch_up: Option<String>,

    /// MANUAL: size | pnl | tier (default size)
    #[arg(long)]
    axis: Option<String>,
    /// MANUAL: realized | realized-pit | unrealized (default realized); pnl ONLY, size and tier rank
    /// no PnL, and passing one there is refused
    #[arg(long)]
    grading: Option<String>,
    /// PERIODIC: N days back from now
    #[arg(long)]
    days: Option<String>,
    /// AD-HOC: window start, YYYY-MM-DD | YYYY-MM-DDTHH | unix seconds (inclusive)
    #[arg(long)]
    start: Option<String>,
    /// AD-HOC: window end, same forms (inclusive). Both ends are floored to the hour
    #[arg(long)]
    end: Option<String>,

    /// PANEL: fetch (ts, premium, open_interest) per hour from /v1/{exchange}/assets/hourly and
    /// ingest it as `kind=perp_metrics` under the `perp_panel:{venue}:{asset}:{start}-{end}` commit
    /// key. AD-HOC ONLY (--start/--end required). Writes the SAME kind as the datahub funding
    /// lane's premium rows, so one store must take one producer for a window or the shared hours
    /// land twice; crates/vike-backfill/src/vikedata/panel.rs carries the argument. This is the ONLY
    /// producer that fills open_interest.
    #[arg(long)]
    panel: bool,
}

/// Its own function so the environment sweep, the store open and the credential refusal are spelled
/// once here and once in the cohort path rather than interleaved — the two shapes share nothing
/// past the argument list.
fn run_panel(
    store: Option<&str>,
    api_base_arg: Option<&str>,
    asset: &str,
    exchange: &str,
    start_secs: i64,
    end_secs: i64,
) -> ExitCode {
    let env: HashMap<String, String> = std::env::vars().collect();
    let root = store_root(store, &env);
    let base = api_base(api_base_arg);
    let creds = match scoped_credentials(&env, &KeyScope::of(SCOPE)) {
        Ok(creds) => creds,
        Err(e) => {
            eprintln!("vikedata_backfill: {e}");
            return ExitCode::FAILURE;
        }
    };
    let api_key = match api_key(&creds) {
        Ok(Some(key)) => key,
        Ok(None) => {
            eprintln!(
                "{API_KEY_ENV} is not in the credential store, so this run is REFUSED. An \
                 unauthenticated fetch 403s part-way through the window and would store a partial \
                 batch under a commit key claiming the whole one."
            );
            return ExitCode::FAILURE;
        }
        Err(undeclared) => {
            eprintln!("credential scope defect: {undeclared}");
            return ExitCode::FAILURE;
        }
    };
    let store = match DataFusionHist::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("open store {}: {e}", root.display());
            return ExitCode::FAILURE;
        }
    };
    let got = match fetch_panel(&api_key, &base, exchange, asset, start_secs, end_secs) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("panel fetch {exchange}/{asset}: {e}");
            return ExitCode::FAILURE;
        }
    };
    if got.skipped_null_premium > 0 {
        tracing::warn!(
            asset,
            skipped = got.skipped_null_premium,
            "panel hours with a null premium were skipped (their open_interest goes with them)"
        );
    }
    let key = panel_commit_key(exchange, asset, start_secs, end_secs);
    match vike_data::HistStore::append_perp_metrics(&store, exchange, asset, &got.rows, Some(&key))
    {
        Ok(written) => {
            let with_oi = got.rows.iter().filter(|r| r.open_interest.is_some()).count();
            tracing::info!(
                exchange,
                asset,
                rows = got.rows.len(),
                written,
                with_oi,
                skipped_null_premium = got.skipped_null_premium,
                "vikedata panel backfill complete"
            );
            println!(
                "{written} rows -> kind=perp_metrics venue={exchange} symbol={asset} \
                 (open_interest on {with_oi}) window=[{start_secs}, {end_secs}]"
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("append perp_metrics {exchange}/{asset}: {e}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    // ⚠ FIRST — `--help`, `--version` and a usage error answer before any log file, store or call.
    let Args {
        asset,
        exchange,
        store: store_arg,
        api_base: api_base_arg,
        schedule,
        catch_up,
        axis: axis_arg,
        grading: grading_arg,
        days,
        start,
        end,
        panel,
    } = parse("vikedata_backfill");
    // ONE process-environment sweep, at the root: the log level, the hist-store root, the API base
    // and the credential chain's settings-directory override all come out of it.
    let env: HashMap<String, String> = std::env::vars().collect();
    let _guards = vike_log::init(log_config("vikedata-backfill", &env));

    // The clock is read HERE and passed in: the library resolves every window against ONE instant,
    // so a multi-asset sweep cannot end up with N slightly different windows.
    let now_secs = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => {
            eprintln!("system clock is before the unix epoch: {e}");
            return ExitCode::FAILURE;
        }
    };

    // The PANEL shape: ad-hoc only, no ladder — it fetches (premium, open_interest) hours, not a
    // cohort ladder, so every ladder/schedule flag is a usage error with it.
    if panel {
        if schedule
            || days.is_some()
            || axis_arg.is_some()
            || grading_arg.is_some()
            || catch_up.is_some()
        {
            eprintln!(
                "vikedata_backfill: --panel takes only --start/--end — it fetches no ladder and \
                 may not ride the schedule (see --help)"
            );
            return ExitCode::from(2);
        }
        let (Some(s), Some(e)) = (start.as_deref(), end.as_deref()) else {
            eprintln!("vikedata_backfill: --panel needs --start and --end (see --help)");
            return ExitCode::from(2);
        };
        let (Some(start_secs), Some(end_secs)) = (boundary_secs(s), boundary_secs(e)) else {
            eprintln!(
                "vikedata_backfill: --start/--end must be dates or unix seconds (see --help)"
            );
            return ExitCode::from(2);
        };
        if end_secs <= start_secs {
            eprintln!("vikedata_backfill: --end must be after --start");
            return ExitCode::from(2);
        }
        return run_panel(
            store_arg.as_deref(),
            api_base_arg.as_deref(),
            &asset,
            &exchange,
            start_secs,
            end_secs,
        );
    }
    let plan = match resolve_plan(
        &WindowArgs {
            schedule,
            catch_up: catch_up.as_deref(),
            days: days.as_deref(),
            start: start.as_deref(),
            end: end.as_deref(),
            axis: axis_arg.as_deref(),
            grading: grading_arg.as_deref(),
        },
        now_secs,
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("vikedata_backfill: {e} (see --help)");
            return ExitCode::from(2);
        }
    };

    let root = store_root(store_arg.as_deref(), &env);
    let base = api_base(api_base_arg.as_deref());

    let creds = match scoped_credentials(&env, &KeyScope::of(SCOPE)) {
        Ok(creds) => creds,
        Err(e) => {
            eprintln!("vikedata_backfill: {e}");
            return ExitCode::FAILURE;
        }
    };
    let api_key = match api_key(&creds) {
        Ok(Some(key)) => key,
        Ok(None) => {
            eprintln!(
                "{API_KEY_ENV} is not in the credential store, so this run is REFUSED. An \
                 unauthenticated fetch 403s part-way through the window and would store a partial \
                 batch under a commit key claiming the whole one."
            );
            return ExitCode::FAILURE;
        }
        Err(undeclared) => {
            eprintln!("credential scope defect: {undeclared}");
            return ExitCode::FAILURE;
        }
    };

    let store = match DataFusionHist::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("open store {}: {e}", root.display());
            return ExitCode::FAILURE;
        }
    };
    let rec = CohortRecorder::new(Arc::new(store));

    // The two shapes are ONE loop over `(ladder, window)` pairs: the manual shapes are the
    // single-pair case. Spelling them separately is how the schedule would come to differ from the
    // command an operator debugs it with.
    let attempts: Vec<(Ladder, CohortWindow)> = match &plan {
        RunPlan::AdHoc { axis, grading, window } => {
            vec![(Ladder { axis: *axis, grading: *grading }, *window)]
        }
        RunPlan::Scheduled { windows } => {
            windows.iter().flat_map(|w| POLLED.iter().map(move |l| (*l, *w))).collect()
        }
    };

    let mut failed = 0usize;
    let mut rows_total = 0usize;
    for (ladder, window) in &attempts {
        // ⚠ EVERY attempt runs. One ladder failing must not starve the others: each is its own
        // commit key, so a partial run leaves the store consistent, and the ladder that failed is
        // re-attempted by the next firing while `--catch-up` still covers its day. Aborting here
        // would let one stalled grading (see schedule.rs's DEFERRED note on the staleness guard)
        // silently stop collecting the ladders that are healthy.
        match backfill_cohort(
            &rec,
            &api_key,
            &base,
            &exchange,
            &asset,
            ladder.axis,
            ladder.grading,
            window,
        ) {
            Ok(n) => {
                rows_total += n;
                println!(
                    "{n} rows -> kind=cohort venue={exchange} symbol={asset} ladder={} \
                     window=[{}, {}]",
                    ladder.label(),
                    window.start_secs,
                    window.anchor_secs
                );
            }
            Err(e) => {
                failed += 1;
                eprintln!(
                    "backfill failed: ladder={} window=[{}, {}]: {e}",
                    ladder.label(),
                    window.start_secs,
                    window.anchor_secs
                );
            }
        }
    }

    if matches!(plan, RunPlan::Scheduled { .. }) {
        // One greppable verdict line, because a timer's whole output is what `journalctl` shows.
        println!(
            "vikedata schedule: venue={exchange} symbol={asset} attempts={} ok={} failed={failed} \
             rows={rows_total}",
            attempts.len(),
            attempts.len() - failed
        );
    }
    if failed > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2025-08-09T13:00:00Z.
    const ANCHOR: i64 = 1_754_744_400;

    #[test]
    fn a_boundary_parses_as_a_date_an_hour_or_bare_unix_seconds() {
        assert_eq!(boundary_secs("2025-08-09"), Some(ANCHOR - 13 * 3_600));
        assert_eq!(boundary_secs("2025-08-09T13"), Some(ANCHOR));
        assert_eq!(boundary_secs(" 1754744400 "), Some(ANCHOR));
        assert_eq!(boundary_secs("2025-13-01"), None, "an out-of-range month is not a date");
        assert_eq!(boundary_secs("last tuesday"), None);
        assert_eq!(boundary_secs(""), None);
    }

    #[test]
    fn days_resolves_the_periodic_shape_and_start_end_the_ad_hoc_one() {
        let trailing = resolve_window(Some("30"), None, None, ANCHOR + 59).unwrap();
        assert!(!trailing.pinned);
        assert_eq!(trailing.days, 30);
        assert_eq!(trailing.anchor_secs, ANCHOR, "the clock is hour-floored");

        let ranged = resolve_window(None, Some("2025-08-01"), Some("2025-08-08"), ANCHOR).unwrap();
        assert!(ranged.pinned, "an ad-hoc range pins BOTH ends — the endpoint serves past --end");
        assert_eq!(ranged.days, 7);
    }

    #[test]
    fn the_two_run_shapes_are_never_mixed_or_guessed_at() {
        for (d, s, e) in [
            (Some("7"), Some("2025-08-01"), Some("2025-08-08")),
            (Some("7"), Some("2025-08-01"), None),
            (Some("7"), None, Some("2025-08-08")),
        ] {
            let err = resolve_window(d, s, e, ANCHOR).unwrap_err();
            assert!(err.contains("two different run shapes"), "{err}");
        }
        assert!(
            resolve_window(None, Some("2025-08-01"), None, ANCHOR).unwrap_err().contains("--end")
        );
        assert!(
            resolve_window(None, None, Some("2025-08-08"), ANCHOR).unwrap_err().contains("--start")
        );
        assert!(resolve_window(None, None, None, ANCHOR).unwrap_err().contains("--days"));
    }

    #[test]
    fn a_zero_or_non_numeric_days_is_a_usage_error_rather_than_an_empty_window() {
        assert!(resolve_window(Some("0"), None, None, ANCHOR).unwrap_err().contains("at least 1"));
        assert!(resolve_window(Some("-3"), None, None, ANCHOR).is_err());
        assert!(resolve_window(Some("thirty"), None, None, ANCHOR).is_err());
    }

    // ---- the SCHEDULED shape -----------------------------------------------------------------

    fn scheduled(catch_up: Option<&str>) -> WindowArgs<'_> {
        WindowArgs { schedule: true, catch_up, ..WindowArgs::default() }
    }

    #[test]
    fn the_scheduled_shape_plans_the_declared_ladders_over_complete_utc_days() {
        let RunPlan::Scheduled { windows } = resolve_plan(&scheduled(Some("3")), ANCHOR).unwrap()
        else {
            panic!("--schedule must plan the scheduled shape");
        };
        assert_eq!(windows.len(), 3);
        assert!(windows.iter().all(|w| w.pinned && w.days == 1), "{windows:?}");
        assert!(
            windows.iter().all(|w| w.anchor_secs < ANCHOR - 13 * 3_600),
            "the still-filling day is never planned: {windows:?}"
        );
        // The look-back defaults to one day rather than to nothing or to everything.
        let RunPlan::Scheduled { windows } = resolve_plan(&scheduled(None), ANCHOR).unwrap() else {
            panic!("default catch-up must still be the scheduled shape");
        };
        assert_eq!(windows.len(), DEFAULT_CATCH_UP as usize);
    }

    /// ⚠ THE refusal the schedule's safety rests on. `--schedule --days 30` would be the sliding
    /// trailing window the grid exists to replace, wearing the safe shape's name — and the shared
    /// hours of two firings land TWICE, because idempotency is per commit KEY, never per row.
    #[test]
    fn a_scheduled_run_cannot_be_handed_a_boundary_of_any_kind() {
        for (flag, args) in [
            ("--days", WindowArgs { days: Some("30"), ..scheduled(None) }),
            ("--start", WindowArgs { start: Some("2025-08-01"), ..scheduled(None) }),
            ("--end", WindowArgs { end: Some("2025-08-08"), ..scheduled(None) }),
        ] {
            let err = resolve_plan(&args, ANCHOR).unwrap_err();
            assert!(err.contains(flag), "{err}");
            assert!(err.contains("two different run shapes"), "{err}");
        }
    }

    /// …and it cannot be handed a LADDER either: the polled set is a reviewed table with a written
    /// reason on every ladder it leaves out, and a unit file retyping one would be a second copy of
    /// that decision in the place least likely to be revisited.
    #[test]
    fn a_scheduled_run_cannot_be_handed_an_axis_or_a_grading() {
        for (flag, args) in [
            ("--axis", WindowArgs { axis: Some("tier"), ..scheduled(None) }),
            ("--grading", WindowArgs { grading: Some("realized-pit"), ..scheduled(None) }),
        ] {
            let err = resolve_plan(&args, ANCHOR).unwrap_err();
            assert!(err.contains(flag), "{err}");
            assert!(err.contains("POLLED"), "{err}");
        }
    }

    #[test]
    fn the_look_back_belongs_to_the_scheduled_shape_and_is_bounded() {
        let orphan = resolve_plan(
            &WindowArgs { catch_up: Some("3"), days: Some("7"), ..WindowArgs::default() },
            ANCHOR,
        )
        .unwrap_err();
        assert!(orphan.contains("--schedule"), "{orphan}");

        assert!(resolve_plan(&scheduled(Some("0")), ANCHOR).unwrap_err().contains("at least 1"));
        assert!(
            resolve_plan(&scheduled(Some("many")), ANCHOR)
                .unwrap_err()
                .contains("not a whole number")
        );
        let over = MAX_CATCH_UP + 1;
        assert!(
            resolve_plan(&scheduled(Some(&over.to_string())), ANCHOR)
                .unwrap_err()
                .contains(&MAX_CATCH_UP.to_string())
        );
    }

    /// The manual shapes still resolve exactly one operator-chosen ladder, and the axis/grading
    /// parsing that moved into `resolve_plan` still refuses a guess rather than defaulting.
    #[test]
    fn a_manual_run_resolves_one_ladder_and_refuses_an_unknown_spelling() {
        let plan = resolve_plan(
            &WindowArgs {
                axis: Some("pnl"),
                grading: Some("unrealized"),
                start: Some("2025-08-01"),
                end: Some("2025-08-08"),
                ..WindowArgs::default()
            },
            ANCHOR,
        )
        .unwrap();
        let RunPlan::AdHoc { axis, grading, window } = plan else { panic!("{plan:?}") };
        assert_eq!((axis, grading), (Axis::Pnl, Grading::Unrealized));
        assert!(window.pinned);

        let bad_axis = WindowArgs { axis: Some("SIZE"), days: Some("7"), ..WindowArgs::default() };
        assert!(resolve_plan(&bad_axis, ANCHOR).unwrap_err().contains("unknown --axis"));
        let bad_grading =
            WindowArgs { grading: Some("realized_pit"), days: Some("7"), ..WindowArgs::default() };
        assert!(resolve_plan(&bad_grading, ANCHOR).unwrap_err().contains("unknown --grading"));
        // …and a run with no window shape at all now names all three.
        let none = resolve_plan(&WindowArgs::default(), ANCHOR).unwrap_err();
        assert!(none.contains("--schedule") && none.contains("--days"), "{none}");
    }

    fn parsed(v: &[&str]) -> Result<Args, vike_util::cli::CliExit> {
        vike_backfill::cli::try_parse_from("vikedata_backfill", v.iter().copied())
    }

    /// The deployed unit's ExecStart (`deploy/vike-cohort-collector@.service`), in both spellings.
    #[test]
    fn the_deployed_schedule_line_reads_in_both_spellings() {
        for argv in [
            &["--asset", "BTC", "--schedule", "--catch-up", "3", "--store", "/srv/hist"][..],
            &["--asset=BTC", "--schedule", "--catch-up=3", "--store=/srv/hist"][..],
        ] {
            let a = parsed(argv).expect("the deployed line parses");
            assert_eq!(a.asset, "BTC");
            assert!(a.schedule && !a.panel);
            assert_eq!(a.catch_up.as_deref(), Some("3"));
            assert_eq!(a.store.as_deref(), Some("/srv/hist"));
            assert_eq!(a.exchange, DEFAULT_EXCHANGE, "an unmentioned exchange keeps its default");
        }
    }

    /// The `--asset` is the one required flag, and every other manual flag reads.
    #[test]
    fn asset_is_required_and_the_manual_flags_read() {
        let e = parsed(&["--schedule"]).expect_err("--asset is required");
        assert_eq!(e.code, 2);
        assert!(e.text.contains("--asset"), "{}", e.text);
        let a = parsed(&[
            "--asset",
            "BTC",
            "--exchange",
            "bybit",
            "--axis",
            "pnl",
            "--grading",
            "unrealized",
            "--start",
            "2026-04-01",
            "--end",
            "2026-04-08",
            "--api-base",
            "http://h/v1",
            "--panel",
        ])
        .expect("every manual flag parses");
        assert_eq!(a.exchange, "bybit");
        assert_eq!((a.axis.as_deref(), a.grading.as_deref()), (Some("pnl"), Some("unrealized")));
        assert_eq!(a.api_base.as_deref(), Some("http://h/v1"));
        assert!(a.panel);
    }

    #[test]
    fn an_unknown_flag_is_rejected_rather_than_ignored() {
        // The `--stroe /data` class: a lookup-shaped parser writes to the default store in silence.
        let e = parsed(&["--asset", "BTC", "--stroe", "/data"]).expect_err("a typo must not run");
        assert_eq!(e.code, 2);
        assert!(e.text.contains("--stroe"), "{}", e.text);
        // A switch given a value is refused too (`--schedule=1`).
        assert!(parsed(&["--asset", "BTC", "--schedule=1"]).is_err());
    }

    #[test]
    fn api_base_defaults_to_the_vendor_root() {
        assert_eq!(api_base(None), VIKEDATA_BASE);
        assert_eq!(api_base(Some("http://h/v1")), "http://h/v1");
    }
}

/// **The declared credential SCOPE, exercised through this binary's own resolver.**
///
/// Owner ruling 2026-09-16: a process materialises only what it asked for. The hazard that
/// introduces is that a name this binary forgot to DECLARE would otherwise look exactly like a name
/// that is not in the store — and in this workspace an absent credential is the LIVE GATE, so a
/// forgotten declaration would degrade in silence instead of failing.
///
/// Both tests call the real `api_key`, so neither restates the key name: the name under test is
/// whatever that function actually asks for.
#[cfg(test)]
mod scope_tests {
    use super::*;
    use vike_secrets::{KeyScope, ScopedSecrets, Source};

    /// ⚠ **The mutation target for "a migrated caller's declared set omits a name it needs".**
    ///
    /// Dropping the name from `SCOPE` makes this red, naming the scope — instead of the binary
    /// quietly reporting the credential missing on a box where it IS configured.
    #[test]
    fn the_declared_scope_covers_the_name_this_binary_reads() {
        let scoped = ScopedSecrets::empty(&KeyScope::of(SCOPE), Source::None);
        match api_key(&scoped) {
            Ok(_) => {}
            Err(undeclared) => panic!(
                "this binary reads a credential its own SCOPE does not declare: {undeclared}"
            ),
        }
    }

    /// ...and the other direction, so the test above is not vacuous: an UNDECLARED name really is a
    /// distinct answer from an absent one, at this call site and not merely inside the store crate.
    #[test]
    fn a_name_outside_the_scope_refuses_rather_than_reading_as_absent() {
        // ⚠ A name with no `vike_model::scan` prefix, deliberately: an env-SHAPED literal here would be
        // a sighting in a `src/bin` file, which that scanner scores `Layer::Binary` (a `#[cfg(test)]`
        // module under `src/bin` is NOT test-classified), and would demand a registry row asserting
        // a read this binary does not make. MEASURED — the first spelling used `VIKE_SETTINGS_DIR`
        // and reddened `declared_layer_matches_the_path` in five bins at once.
        let foreign =
            ScopedSecrets::empty(&KeyScope::of(["A_NAME_THIS_BINARY_DOES_NOT_READ"]), Source::None);
        assert!(
            api_key(&foreign).is_err(),
            "a scope that declares something else must REFUSE, never answer `no credential` — \
             absent credentials are the live gate and this is not that"
        );
    }
}
