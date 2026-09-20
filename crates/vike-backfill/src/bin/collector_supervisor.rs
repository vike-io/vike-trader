//! The always-on collector supervisor: keep a declared set of hist-store series fresh and heal
//! their historical holes, forever, instead of an operator re-running one-shot `*_backfill` bins.
//!
//!   cargo run -p vike-backfill --bin collector_supervisor -- \
//!       --config collectors.toml [--store DIR] [--status FILE] [--once]
//!
//! `--config` is required (see `vike_backfill::supervisor::config` for the TOML shape). `--store`
//! defaults to `$VIKE_HIST_STORE` else `<repo>/market_data/hist` (the shared `cli::store_root` law), and a
//! `store =` line in the config is the lowest-precedence fallback. `--status` (or the config's
//! `status_file =`) publishes the JSON status surface — last-run / next-run / heal-queue /
//! last-error per source — rewritten atomically after every pass. `--once` runs a single pass and
//! exits, for a cron/systemd-timer deployment instead of a resident process.
//!
//! SHUTDOWN: the resident mode blocks on stdin and stops when stdin closes (EOF) or the operator
//! types `quit`, then `stop()`s the worker — flag, wake, join — so the in-flight pass finishes
//! cleanly. Ctrl-C also ends the process; nothing is lost either way, because every store write is
//! already committed (and idempotent) by the time a pass returns.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use vike_backfill::cli::{CliSpec, arg, has_flag, log_config, store_root};
use vike_backfill::supervisor::{CollectorSupervisor, load_supervisor_config, run_once};
use vike_data::DataFusionHist;

const USAGE: &str = "\
usage: collector_supervisor --config FILE.toml [--store DIR] [--status FILE] [--once]

Keep a declared set of hist-store series fresh and heal their historical holes, forever, instead of
an operator re-running one-shot *_backfill bins. Resident by default; every store write is already
committed and idempotent by the time a pass returns, so nothing is lost on a stop.

  --config FILE   the collector roster TOML (required; see supervisor::config for the shape)
  --store DIR     hist-store root. Precedence: --store, then $VIKE_HIST_STORE, then the config
                  `store =` line, then <repo>/market_data/hist.
  --status FILE   publish the JSON status surface (last-run / next-run / heal-queue / last-error
                  per source), rewritten atomically after every pass. The config `status_file =`
                  is the fallback.
  --once          run a SINGLE pass and exit — for a cron or systemd-timer deployment instead of
                  a resident process
  -h, --help      print this and exit 0
  -V, --version   print the version and exit 0

The resident mode stops on EOF on stdin or the word `quit`, finishing the in-flight pass first.";

const SPEC: CliSpec = CliSpec {
    bin: "collector_supervisor",
    usage: USAGE,
    valued: &["--config", "--store", "--status"],
    toggles: &["--once"],
    positionals: 0,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    SPEC.short_circuit_or_exit(&args);
    let _log_guards = vike_log::init(log_config("collector-supervisor"));

    let Some(config_path) = arg(&args, "--config") else {
        eprintln!("collector_supervisor: --config is required\n\n{USAGE}");
        std::process::exit(2);
    };
    let cfg = match load_supervisor_config(Path::new(&config_path)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("collector_supervisor: {e}");
            std::process::exit(2);
        }
    };

    let store_flag = arg(&args, "--store");
    let root =
        store_root(store_flag.as_deref().or(cfg.store.as_deref()), &std::env::vars().collect());
    let status_path: Option<PathBuf> =
        arg(&args, "--status").or_else(|| cfg.status_file.clone()).map(PathBuf::from);

    let hist = match DataFusionHist::open(&root) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("collector_supervisor: open store {root:?}: {e}");
            std::process::exit(1);
        }
    };

    tracing::info!(
        "collector-supervisor: {} source(s), tick {}s, store {root:?}, status {status_path:?}",
        cfg.sources.len(),
        cfg.tick_secs
    );

    if has_flag(&args, "--once") {
        match run_once(&hist, &cfg, status_path.as_deref()) {
            Ok(status) => {
                for row in &status.sources {
                    tracing::info!(
                        "{}: passes={} rows={} heal_queue={} failures={} heal_failures={} err={:?}",
                        row.name,
                        row.passes,
                        row.rows_ingested,
                        row.heal_queue,
                        row.consecutive_failures,
                        row.heal_failures,
                        row.last_error
                    );
                }
                tracing::info!("collector-supervisor: single pass complete");
            }
            Err(e) => {
                eprintln!("collector_supervisor: {e}");
                std::process::exit(2);
            }
        }
        return;
    }

    let mut sup = match CollectorSupervisor::start(Arc::new(hist), cfg, status_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("collector_supervisor: {e}");
            std::process::exit(2);
        }
    };

    // Block until stdin closes (EOF) or the operator types `quit`, then tear down deterministically.
    let stdin = std::io::stdin();
    let mut line = String::new();
    loop {
        line.clear();
        // Bind the read BEFORE matching, so the `&mut line` borrow is definitively over by the time
        // the arms read `line` back.
        let read = stdin.read_line(&mut line);
        match read {
            Ok(0) => break, // EOF
            Ok(_) => {
                if line.trim().eq_ignore_ascii_case("quit") {
                    break;
                }
            }
            Err(e) => {
                tracing::warn!("collector-supervisor: stdin read failed ({e}) — shutting down");
                break;
            }
        }
    }
    sup.stop();
    tracing::info!("collector-supervisor: stopped after {} pass(es)", sup.passes_completed());
}
