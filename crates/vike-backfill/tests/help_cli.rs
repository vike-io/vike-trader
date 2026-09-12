//! **Every `vike-backfill` bin answers `--help` with exit 0 and its usage on STDOUT** — asserted
//! over the SHIPPED binaries, because only a real run shows a status and a stream.
//!
//! None of the 25 bins in this crate recognised `-h`/`--help`/`--version`/`-V`. Surveyed on
//! `9e11d8ab`, `--help` produced: a usage line on stderr with exit 1 (14 bins); exit 2, five of
//! them with no usage at all because a `--flag` loop parser reported `unknown arg: --help` (9
//! bins); a **panic**, exit 101 (`poly_reparse`, whose six required flags were `.expect`ed); and —
//! the reason this is not cosmetic — **a completed data ingest** (`ingest_bench_bars`, which read
//! no argv whatsoever, so `--help` opened a store and wrote bars). `--help` is the first thing a
//! person types to find out what a command does and the safest-looking invocation there is.
//!
//! ⚠ **Every spawn is DEADLINED, and that is not defensive decoration.** The sibling gate this one
//! copies (`vike-datahub`'s `tests/help_cli.rs`) exists because `vike-datahub --help` used to bind
//! a listener and serve, so `Command::output()` — which blocks until the child exits — HUNG. The
//! same failure is available here: several of these bins open a store, spawn `clickhouse-client`
//! or start paging an HTTP archive. A regression must fail in 30 s with a diagnosis, not park a CI
//! job.
//!
//! ⚠ **This test must never perform any work**: no store opened, no network call, no ingest. That
//! is the property under test, so a bin doing any of it would be the defect, not the harness.
//! [`help_does_no_work`] asserts the strongest available witness for the worst offender —
//! `ingest_bench_bars --help` must leave `market_data/bench_hist` exactly as it found it.
//!
//! **Coverage is machine-checked.** Bins live behind several different Cargo features, so the table
//! below is split by feature and `every_bin_in_this_crate_is_covered` walks `src/bin/` and fails
//! if a file is missing from it — a new bin cannot silently escape the contract. (The count of
//! features is deliberately not written down: it was "six" and went stale the first time one was
//! added, which is the defect this whole file exists to gate one level up.)

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// How long a `--help`/`--version`/usage-error invocation may take. Enormous for what it measures
/// (these finish in milliseconds) because it is not a performance budget — it is the line between
/// "exited" and "is doing the job it was asked to describe".
const EXIT_DEADLINE: Duration = Duration::from_secs(30);

/// The flag no bin accepts, used for the negative half of the contract.
const BOGUS: &str = "--vike-not-a-flag";

// ── the roster, split by the Cargo feature that builds each bin ──────────────────────────────────
//
// `env!("CARGO_BIN_EXE_<name>")` only resolves for a bin this build actually produced, so a
// `required-features` bin has to sit behind the matching `#[cfg(feature = …)]` or the test file
// will not compile in a default build.

/// No `required-features`: built by every `cargo test -p vike-backfill`, including CI's fast lane.
const DEFAULT_BINS: &[(&str, &str)] = &[
    (env!("CARGO_BIN_EXE_clickhouse_poly_backfill"), "clickhouse_poly_backfill"),
    (env!("CARGO_BIN_EXE_clickhouse_spot_backfill"), "clickhouse_spot_backfill"),
    (env!("CARGO_BIN_EXE_eod_backfill"), "eod_backfill"),
    (env!("CARGO_BIN_EXE_exec_trade_backfill"), "exec_trade_backfill"),
    (env!("CARGO_BIN_EXE_ingest_bench_bars"), "ingest_bench_bars"),
    (env!("CARGO_BIN_EXE_migrate_to_group"), "migrate_to_group"),
    (env!("CARGO_BIN_EXE_pmxt_backfill"), "pmxt_backfill"),
];

#[cfg(feature = "venue-backfill")]
const VENUE_BINS: &[(&str, &str)] = &[
    (env!("CARGO_BIN_EXE_aster_backfill"), "aster_backfill"),
    (env!("CARGO_BIN_EXE_binance_backfill"), "binance_backfill"),
    (env!("CARGO_BIN_EXE_bybit_backfill"), "bybit_backfill"),
    (env!("CARGO_BIN_EXE_collector_supervisor"), "collector_supervisor"),
    (env!("CARGO_BIN_EXE_deribit_backfill"), "deribit_backfill"),
    (env!("CARGO_BIN_EXE_dukascopy_backfill"), "dukascopy_backfill"),
    (env!("CARGO_BIN_EXE_funding_rate_backfill"), "funding_rate_backfill"),
    (env!("CARGO_BIN_EXE_hyperliquid_backfill"), "hyperliquid_backfill"),
    (env!("CARGO_BIN_EXE_hyperliquid_funding_backfill"), "hyperliquid_funding_backfill"),
    (env!("CARGO_BIN_EXE_okx_backfill"), "okx_backfill"),
];

#[cfg(feature = "poly-reparse")]
const POLY_REPARSE_BINS: &[(&str, &str)] = &[(env!("CARGO_BIN_EXE_poly_reparse"), "poly_reparse")];

#[cfg(feature = "ibkr")]
const IBKR_BINS: &[(&str, &str)] = &[(env!("CARGO_BIN_EXE_ibkr_backfill"), "ibkr_backfill")];

#[cfg(feature = "databento")]
const DATABENTO_BINS: &[(&str, &str)] =
    &[(env!("CARGO_BIN_EXE_databento_backfill"), "databento_backfill")];

#[cfg(feature = "tardis")]
const TARDIS_BINS: &[(&str, &str)] = &[(env!("CARGO_BIN_EXE_tardis_backfill"), "tardis_backfill")];

#[cfg(feature = "vikedata")]
const VIKEDATA_BINS: &[(&str, &str)] =
    &[(env!("CARGO_BIN_EXE_vikedata_backfill"), "vikedata_backfill")];

#[cfg(feature = "poly-ch-backtest")]
const POLY_CH_BINS: &[(&str, &str)] = &[
    (env!("CARGO_BIN_EXE_poly_ch_backtest"), "poly_ch_backtest"),
    (env!("CARGO_BIN_EXE_poly_mm_batch"), "poly_mm_batch"),
];

#[cfg(feature = "vike-archive")]
const ARCHIVE_BINS: &[(&str, &str)] = &[
    (env!("CARGO_BIN_EXE_events_api_backfill"), "events_api_backfill"),
    (env!("CARGO_BIN_EXE_vike_archive_backfill"), "vike_archive_backfill"),
];

/// Every bin THIS build produced. The feature lanes each cover a different slice; CI's union
/// `backfill` lane covers all but `ibkr`, which has its own lane.
fn bins() -> Vec<(&'static str, &'static str)> {
    // `mut` is used only by the `extend_from_slice`s below, every one of which is behind a feature
    // — a DEFAULT build genuinely needs none of them, and CI's `-D warnings` would fail on the
    // unused `mut` there. The allow is narrower than dropping the tables.
    #[allow(unused_mut)]
    let mut all: Vec<(&str, &str)> = DEFAULT_BINS.to_vec();
    #[cfg(feature = "venue-backfill")]
    all.extend_from_slice(VENUE_BINS);
    #[cfg(feature = "poly-reparse")]
    all.extend_from_slice(POLY_REPARSE_BINS);
    #[cfg(feature = "ibkr")]
    all.extend_from_slice(IBKR_BINS);
    #[cfg(feature = "databento")]
    all.extend_from_slice(DATABENTO_BINS);
    #[cfg(feature = "tardis")]
    all.extend_from_slice(TARDIS_BINS);
    #[cfg(feature = "vikedata")]
    all.extend_from_slice(VIKEDATA_BINS);
    #[cfg(feature = "poly-ch-backtest")]
    all.extend_from_slice(POLY_CH_BINS);
    #[cfg(feature = "vike-archive")]
    all.extend_from_slice(ARCHIVE_BINS);
    all
}

/// Run a shipped bin and REQUIRE it to exit. A child still alive at [`EXIT_DEADLINE`] is killed and
/// reported as the defect it is: a binary answering a question about its command line by doing the
/// work instead.
fn run(exe: &str, name: &str, args: &[&str]) -> Output {
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {name} {args:?}: {e}"));

    let deadline = Instant::now() + EXIT_DEADLINE;
    loop {
        match child.try_wait().expect("poll the child") {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "`{name} {}` did not exit within {EXIT_DEADLINE:?} — it is still RUNNING, which \
                     for these bins means it opened a store, spawned a subprocess or started \
                     fetching instead of answering. Answering the command line must do no work.",
                    args.join(" ")
                );
            }
            // Polling rather than a blocking wait: `wait_with_output` cannot be deadlined, and the
            // output here is far too small to fill a pipe while we sleep.
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    child.wait_with_output().expect("collect the exited child's output")
}

/// `--help` and `-h`: exit 0, usage on STDOUT, nothing on stderr.
///
/// The empty-stderr clause is load-bearing beyond tidiness — it is what catches `poly_reparse`'s
/// old `.expect` PANIC, whose backtrace went there.
#[test]
fn help_exits_zero_with_usage_on_stdout() {
    for (exe, name) in bins() {
        for flag in ["--help", "-h"] {
            let out = run(exe, name, &[flag]);
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                out.status.success(),
                "`{name} {flag}` must exit 0 — a non-zero --help breaks `set -e` and every \
                 packaging smoke test. status: {:?}, stderr: {stderr}",
                out.status.code()
            );
            assert!(
                stdout.contains("usage:"),
                "`{name} {flag}` must print its usage to STDOUT; stdout: {stdout:?}, \
                 stderr: {stderr:?}"
            );
            assert!(
                stdout.contains(name),
                "`{name} {flag}`'s usage must name the bin it describes; stdout: {stdout:?}"
            );
            assert!(
                stderr.trim().is_empty(),
                "`{name} {flag}`: a successful --help produces no diagnostics and no panic; \
                 stderr: {stderr:?}"
            );
        }
    }
}

/// `--version`/`-V`: the crate version on stdout, exit 0. Unrecognised in every bin before this.
#[test]
fn version_prints_the_crate_version_on_stdout() {
    for (exe, name) in bins() {
        for flag in ["--version", "-V"] {
            let out = run(exe, name, &[flag]);
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                out.status.success(),
                "`{name} {flag}` must exit 0; status: {:?}, stderr: {stderr}",
                out.status.code()
            );
            assert!(
                stdout.contains(env!("CARGO_PKG_VERSION")),
                "`{name} {flag}` must print the version {:?} on stdout; stdout: {stdout:?}",
                env!("CARGO_PKG_VERSION")
            );
            assert!(
                stdout.contains(name),
                "…and name the BIN, not the crate — all 25 share one `CARGO_PKG_NAME`; \
                 stdout: {stdout:?}"
            );
        }
    }
}

/// The negative half, so "exit 0 on --help" is never bought by making everything exit 0: an
/// unrecognised flag is a usage error on stderr with a non-zero status.
///
/// Most of these bins read options with `cli::arg`/`cli::has_flag`, one-flag lookups blind to every
/// token they were not asked about — so `--stroe /data` used to resolve to the DEFAULT store and
/// the backfill wrote to the wrong root with no diagnostic at all.
#[test]
fn an_unknown_argument_is_rejected_rather_than_ignored() {
    for (exe, name) in bins() {
        let out = run(exe, name, &[BOGUS]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "`{name} {BOGUS}` must exit non-zero rather than run; stdout: {stdout:?}"
        );
        assert!(
            stderr.contains(BOGUS),
            "`{name} {BOGUS}`: the error names the offending argument; stderr: {stderr:?}"
        );
        assert!(
            stderr.contains("usage:"),
            "`{name} {BOGUS}`: …and explains itself with the usage; stderr: {stderr:?}"
        );
        assert!(
            stdout.trim().is_empty(),
            "`{name} {BOGUS}`: a usage error writes nothing to stdout; stdout: {stdout:?}"
        );
    }
}

/// **`ingest_bench_bars --help` must perform NO WORK.** This bin read no argv at all: `main` went
/// straight to `DataFusionHist::open` and the ingest loop, so `--help` created a store directory
/// and wrote bars into it.
///
/// The witness is `<repo>/market_data/bench_hist`, the store this bin opens — asserted as UNCHANGED
/// across the run rather than as absent, because a developer box may legitimately have run the
/// real ingest. On a fresh clone (CI) the directory does not exist and the assertion is exact;
/// where it does exist the other three clauses of the contract still bite, and the deadline in
/// [`run`] still proves the process did not sit there working.
#[test]
fn help_does_no_work() {
    let store = bench_hist_root();
    let before = store.exists();
    let out = run(env!("CARGO_BIN_EXE_ingest_bench_bars"), "ingest_bench_bars", &["--help"]);
    assert!(out.status.success(), "status: {:?}", out.status.code());
    assert_eq!(
        store.exists(),
        before,
        "`ingest_bench_bars --help` changed the existence of {} — it opened the store instead of \
         answering. `--help` is the safest-looking invocation there is; it must touch nothing.",
        store.display()
    );
}

/// The bench store `ingest_bench_bars` writes into, resolved exactly as that bin resolves it:
/// `<repo>/market_data/bench_hist`, from this crate's manifest dir at compile time.
fn bench_hist_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("CARGO_MANIFEST_DIR has a repo-root ancestor")
        .join("market_data")
        .join("bench_hist")
}

/// A new bin cannot silently escape the contract: every `src/bin/*.rs` must appear in one of the
/// feature tables above.
///
/// Checked against the SOURCE TREE rather than against `bins()`, which only knows what this build
/// produced — a bin behind a feature this lane did not enable is still required to have a row, it
/// is simply exercised by a different lane. That is the same shape as the venue-roster
/// completeness tests elsewhere in the workspace: the table is checked against the canonical list,
/// not against whatever happened to be reachable.
#[test]
fn every_bin_in_this_crate_is_covered() {
    // Every row of every table, features or not — the point is the DECLARATION, not this build.
    let declared: Vec<&str> = ALL_DECLARED_BINS.to_vec();

    let bin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("bin");
    let mut on_disk: Vec<String> = std::fs::read_dir(&bin_dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", bin_dir.display()))
        .map(|e| e.expect("dir entry").file_name().to_string_lossy().into_owned())
        .filter_map(|f| f.strip_suffix(".rs").map(str::to_string))
        .collect();
    on_disk.sort();

    for name in &on_disk {
        assert!(
            declared.contains(&name.as_str()),
            "`{name}` is a bin in src/bin/ with no row in tests/help_cli.rs. Every bin owes its \
             caller --help/--version and an unknown-argument rejection (see cli::CliSpec); add it \
             to the table for its Cargo feature."
        );
    }
    for name in &declared {
        assert!(
            on_disk.contains(&(*name).to_string()),
            "`{name}` is declared in tests/help_cli.rs but src/bin/{name}.rs no longer exists — \
             delete the row."
        );
    }
}

/// The declared roster, spelled ONCE and independent of which features this build enabled. Kept
/// beside the tables above; `every_bin_in_this_crate_is_covered` is what keeps it honest.
const ALL_DECLARED_BINS: &[&str] = &[
    // default build
    "clickhouse_poly_backfill",
    "clickhouse_spot_backfill",
    "eod_backfill",
    "exec_trade_backfill",
    "ingest_bench_bars",
    "migrate_to_group",
    "pmxt_backfill",
    // venue-backfill
    "aster_backfill",
    "binance_backfill",
    "bybit_backfill",
    "collector_supervisor",
    "deribit_backfill",
    "dukascopy_backfill",
    "funding_rate_backfill",
    "hyperliquid_backfill",
    "hyperliquid_funding_backfill",
    "okx_backfill",
    // one feature each
    "poly_reparse",          // poly-reparse
    "ibkr_backfill",         // ibkr
    "databento_backfill",    // databento
    "tardis_backfill",       // tardis
    "vikedata_backfill",     // vikedata
    "poly_ch_backtest",      // poly-ch-backtest
    "poly_mm_batch",         // poly-ch-backtest
    "events_api_backfill",   // vike-archive
    "vike_archive_backfill", // vike-archive
];
