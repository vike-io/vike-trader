//! `backtest data export`: one series read over the wire, encoded as a standalone Parquet file.

use std::path::Path;
use std::process::ExitCode;

use vike_analytics::binutil::arg;
use vike_datahub_client::route::{history_route, open_routed_history};

use crate::backtest_cli::serve::load_backtest_settings;

/// `data export` — one series out of the store as a standalone Parquet file.
///
/// The producer half of the starter dataset (`scripts/publish_starter_data.sh` calls exactly this),
/// and useful on its own: handing somebody a slice without handing them the store's internals.
///
/// ⚠ Gated on `venue-fetch` until 2026-09-19, which was wrong in both directions: this function
/// reaches no venue, and the feature it named is about reaching one. See the `"export"` arm in
/// [`run_data`] for the measurement and for why nothing gates it now.
///
/// # ⚠ It READS OVER THE WIRE since 2026-09-26, and only the ENCODING is local
///
/// Decision 0084's 2026-09-25 amendment closed the local READ door on every history reader and
/// named this verb as the one it had not closed: it opened `DataFusionHist` at `--store` and called
/// the store's own `export_bars_parquet` (deleted with this change — its only production caller was
/// this function). That is a second reader of the store, whatever file it writes afterwards. So the
/// bars now come from the datahub the SETTINGS name (`config.datahub_addr`) — the same
/// `load_backtest_settings` walk and the same
/// `history_route` + `open_routed_history` pair the run path takes, so one box has one answer to
/// "where is my history" — and `vike_data::write_bars_parquet` encodes them. The file is the same
/// file: that encoder is what the deleted method ran after its own `load_bars`, and the wire
/// carries a `Bar` bit-exactly — `crates/vike-backtest/tests/optimizer_cli/data_export.rs`'s
/// `data_export_reads_its_bars_through_a_datahub_and_writes_what_the_store_holds` holds the
/// exported bars bit-equal to the store read DIRECTLY, through a real datahub.
///
/// ⚠ **The server could not do this half, and that is why the encoding stayed here** rather than
/// the verb moving onto the wire whole: `crates/vike-datahub/src/server.rs` holds an
/// `Arc<dyn HistStore>` and is backend-agnostic by design, so it has no Parquet encoder to drive —
/// the measurement `crates/vike-cli/src/cmd/data/hist/export.rs`'s `parquet_refusal` states for the
/// same reason. What crosses the wire is the ordinary paged `LoadBars`, so nothing about the
/// protocol moved and a datahub that predates this serves it.
///
/// ⚠ **`--store` is refused BEFORE this function runs** ([`refuse_a_store_on_a_data_read`]), so
/// nothing here resolves a store root — and a store that is absent is no longer CREATED by an
/// export, which `DataFusionHist::open`'s `create_dir_all` used to do as a side effect of reading.
pub(super) fn run_export(
    vars: &std::collections::HashMap<String, String>,
    args: &[String],
    raw_spec: &str,
) -> ExitCode {
    let Some(out) = arg(args, "--out") else {
        eprintln!("backtest: --export needs --out FILE");
        return ExitCode::from(2);
    };
    // The same VENUE:SYMBOL:INTERVAL grammar `--fetch` takes, but WITHOUT its venue roster: this
    // reads the store, so any venue the store holds is exportable, including `demo`.
    let parts: Vec<&str> = raw_spec.split(':').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        eprintln!("backtest: --export takes VENUE:SYMBOL:INTERVAL, not {raw_spec:?}");
        return ExitCode::from(2);
    }
    let parse_ts = |s: &str| crate::harness::profile::parse_ts(s).map_err(|e| e.to_string());
    let range = match (arg(args, "--from"), arg(args, "--to")) {
        (None, None) => vike_data::TsRange::all(),
        (from, to) => {
            let conv = |v: Option<String>| -> Result<Option<i64>, String> {
                v.map(|s| parse_ts(&s)).transpose()
            };
            match (conv(from), conv(to)) {
                (Ok(start), Ok(end)) => vike_data::TsRange { start, end },
                (Err(e), _) | (_, Err(e)) => {
                    eprintln!("backtest: {e}");
                    return ExitCode::from(2);
                }
            }
        }
    };

    // Every argv check above is PURE; the settings walk is the first I/O, for the reason the run
    // path gives beside its own call: a refused command line must cost nothing.
    let settings = match load_backtest_settings(vars, "backtest data export") {
        Ok(s) => s,
        Err(code) => return code,
    };
    let route = history_route(settings.config.datahub_addr.as_deref());
    let store = open_routed_history(&route, vars);
    let bars = match store.load_bars(parts[0], parts[1], parts[2], range) {
        Ok(b) => b,
        Err(e) => {
            // The replacement is named on the FAILURE too, not only on the `--store` refusal: the
            // commonest reason this read fails on a fresh box is that no datahub is running at
            // all, and the operator who used to pass a directory needs the one command that serves
            // it rather than a connect error alone.
            eprintln!(
                "backtest data export: reading {raw_spec} from {} failed: {e}\n  For files on this \
                 machine, start a key-less datahub on them first: vike-backend datahub --store DIR \
                 — it binds loopback only, which is where this verb dials by default.",
                route.label()
            );
            return ExitCode::from(2);
        }
    };
    match vike_data::write_bars_parquet(Path::new(&out), &bars) {
        Ok(rows) => {
            // The row count is the load-bearing half of this line: a publishing script that
            // exported an EMPTY slice would otherwise upload a valid file nobody can use. It LEADS
            // the line (`scripts/publish_starter_data.sh` greps `^exported N`); the provenance
            // follows it, because a file whose source is not named is a file nobody can re-derive.
            println!(
                "exported {rows} bars of {}/{} {} to {out} (read from {})",
                parts[0],
                parts[1],
                parts[2],
                route.label()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("backtest: exporting {raw_spec} failed: {e}");
            ExitCode::from(2)
        }
    }
}
