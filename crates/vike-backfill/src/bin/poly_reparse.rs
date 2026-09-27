//! `poly_reparse` — on-demand re-parse of captured Polymarket raw frames into the DataFusion store
//! (raw-first WS frame tap, docs/superpowers/specs/2026-07-11-raw-frame-tap-design.md). Replays the
//! gz frames through the REAL pump (`run_session`) so the regenerated ticks are byte-faithful to a
//! live session, with the captured receive time (ns→ms converted) restored as `local_ts`.
//!
//! Usage:
//!   poly_reparse --raw-dir DIR --store DIR --token ID --from YYYY-MM-DD --to YYYY-MM-DD \
//!                --mode book|quotes|trades --tick-size F
//! (`--tick-size` MUST match the value the live session used, or the book quantizes on a different
//! grid — the operator supplies it; there is no REST fetch in this offline tool.)
//!
//! ⚠ **`poly_reparse --help` used to PANIC** (exit 101 with a backtrace): the six required flags
//! were read with `.expect("--raw-dir required")`, so the most common invocation in the family
//! answered with a crash. Every required flag is now a diagnosed usage error, and the argv triage
//! runs before the first of them.

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64};

use vike_backfill::cli::{CliSpec, arg};
use vike_backfill::reparse::{GzFileStream, LocalTsRewriteSink, RecvNsCell, gz_files_for};
use vike_bridge_core::StreamHealth;
use vike_data::{DataFusionHist, LiveDataSink, RecorderConfig, RecorderSink};
use vike_polymarket::{PumpMode, TokenState, Watchdog, run_session};

const USAGE: &str = "\
usage: poly_reparse --raw-dir DIR --store DIR --token ID --from YYYY-MM-DD --to YYYY-MM-DD
                    [--mode book|quotes|trades] --tick-size F

Re-parse captured Polymarket raw WS frames into the DataFusion hist store, replaying the gz files
through the REAL pump so the regenerated ticks are byte-faithful to a live session. The captured
receive time is restored as each row's local_ts. Offline: no network call, no REST fetch.

  --raw-dir DIR    directory of captured .gz frame files
  --store DIR      hist-store root to write into
  --token ID       the Polymarket token id to re-parse
  --from DATE      first day to replay, YYYY-MM-DD (inclusive)
  --to DATE        last day to replay, YYYY-MM-DD (inclusive)
  --mode MODE      book | quotes | trades (default book)
  --tick-size F    the book's price grid, as a float. REQUIRED, and it MUST match the value the
                   LIVE session used — a different grid re-quantizes the book into wrong prices,
                   and this offline tool has no way to look the right one up.
  -h, --help       print this and exit 0
  -V, --version    print the version and exit 0";

const SPEC: CliSpec = CliSpec {
    bin: "poly_reparse",
    usage: USAGE,
    valued: &["--raw-dir", "--store", "--token", "--from", "--to", "--mode", "--tick-size"],
    toggles: &[],
    positionals: 0,
};

/// A required flag, or a diagnosed usage error — the replacement for six `.expect`s, each of which
/// aborted with a panic message and no usage.
fn required(args: &[String], flag: &str) -> Result<String, String> {
    arg(args, flag).ok_or_else(|| format!("{flag} is required"))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    // ⚠ FIRST: this is the bin whose `--help` used to panic.
    if let Some(code) = SPEC.short_circuit(&args) {
        return code;
    }
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("poly_reparse: {e}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let raw_dir = required(args, "--raw-dir")?;
    let store_dir = required(args, "--store")?;
    let token = required(args, "--token")?;
    let from = required(args, "--from")?;
    let to = required(args, "--to")?;
    let mode = match arg(args, "--mode").as_deref() {
        Some("book") | None => PumpMode::Book,
        Some("quotes") => PumpMode::Quotes,
        Some("trades") => PumpMode::Trades,
        Some(other) => return Err(format!("bad --mode {other} (want book|quotes|trades)")),
    };
    // `PumpMode::as_str` is private, so match the mode to its watchdog/disclosure label here rather
    // than adding a public accessor just for this bin.
    let label = match mode {
        PumpMode::Book => "book",
        PumpMode::Quotes => "quotes",
        PumpMode::Trades => "trades",
    };
    let raw_tick = required(args, "--tick-size")?;
    let tick_size: f64 =
        raw_tick.parse().map_err(|e| format!("bad --tick-size {raw_tick:?}: {e}"))?;

    let files = gz_files_for(std::path::Path::new(&raw_dir), &token, &from, &to);
    if files.is_empty() {
        // Not a USAGE error — the command line was well formed and the answer is simply "nothing
        // matched", so it exits 1 without re-printing the usage.
        eprintln!("poly_reparse: no raw files for {token} in [{from},{to}] under {raw_dir}");
        std::process::exit(1);
    }
    let store = Arc::new(
        DataFusionHist::open(&store_dir).map_err(|e| format!("open store {store_dir}: {e}"))?,
    );
    let (rec, rec_handle) = RecorderSink::spawn(store, RecorderConfig::default())
        .map_err(|e| format!("spawn recorder: {e}"))?;
    let cell: RecvNsCell = Arc::new(AtomicI64::new(0));
    let sink: Arc<dyn LiveDataSink> = Arc::new(LocalTsRewriteSink::new(rec, cell.clone()));

    let mut gz = GzFileStream::new(files, cell);
    let mut state = TokenState::new(tick_size);
    let mut health = StreamHealth::new(60_000);
    let stop = AtomicBool::new(false);
    let mut wd = Watchdog::new(&mut health, label);
    // run_session returns Err(Closed) at EOF of the gz files — the expected clean termination.
    let _ = run_session(&mut gz, sink.as_ref(), &token, mode, &mut state, &stop, &mut wd, || {});
    rec_handle.shutdown();
    eprintln!("poly_reparse: done ({token}, {from}..{to})");
    Ok(())
}
