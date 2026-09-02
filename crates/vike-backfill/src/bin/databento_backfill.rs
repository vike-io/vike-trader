//! Databento historical backfill CLI. Fetches one schema/symbol/date-range from
//! `hist.databento.com` and ingests it into the DataFusion hist store.
//!
//! Usage:
//!   databento_backfill --dataset GLBX.MDP3 --symbol ESZ4 --schema trades \
//!       --start 2024-01-02T00:00:00 --end 2024-01-02T00:01:00 [--store DIR] [--venue NAME]
//!
//! Schema → series: trades|ohlcv-1m|ohlcv-1h|ohlcv-1d|mbp-1|mbp-10.
//! Key: DATABENTO_API_KEY from the credential store (`<project>/settings/secrets.env`),
//! then the process env (never argv).

use std::process::ExitCode;

use vike_backfill::cli::{arg, log_config, scratch_root, store_root, CliSpec};
use vike_backfill::databento::{backfill, DbnKind};
use vike_bridge_core::credentials::load_workspace_secrets_from_env;
use vike_data::DataFusionHist;
use vike_model::scratch::ScratchDir;

/// The Databento key, resolved HERE in the binary (settings STEP 2 — the adapter takes it as a
/// `&str` parameter and reads no environment of its own): the CREDENTIAL STORE
/// (`<project>/settings/secrets.env`) — else the process environment (a CI/local
/// override that touches no gitignored file). `None` ⇒ the live gate in `main` reports and exits.
/// The sibling `tardis_backfill` bin reads its key the same way.
///
/// `store` is the already-resolved credential map, built once in [`main`] from the SAME
/// `std::env::vars()` sweep that resolves the hist-store root — so this bin performs one
/// environment sweep, not two.
fn api_key(store: &std::collections::HashMap<String, String>) -> Option<String> {
    store.get("DATABENTO_API_KEY").cloned().or_else(|| std::env::var("DATABENTO_API_KEY").ok())
}

fn kind_from_schema(schema: &str) -> Option<DbnKind> {
    match schema {
        "trades" => Some(DbnKind::Trades),
        "mbp-1" => Some(DbnKind::QuotesMbp1),
        "mbp-10" => Some(DbnKind::BookMbp10),
        s if s.starts_with("ohlcv-") => Some(DbnKind::Ohlcv(s["ohlcv-".len()..].to_string())),
        _ => None,
    }
}

const USAGE: &str = "\
usage: databento_backfill --dataset D --symbol S --schema SCH --start T --end T
                          [--store DIR] [--venue NAME]

Fetch history from the Databento `timeseries.get_range` CSV API and ingest it into the DataFusion
hist store. Idempotent per window by a `databento:{kind}:{symbol}:{window}` commit key.

  --dataset D    Databento dataset code, e.g. GLBX.MDP3
  --symbol S     the instrument symbol within that dataset
  --schema SCH   trades | ohlcv-1m | ohlcv-1h | ohlcv-1d | mbp-1 | mbp-10
  --start T      window start (Databento timestamp form)
  --end T        window end
  --store DIR    hist-store root (default: $VIKE_HIST_STORE, else <repo>/market_data/hist)
  --venue NAME   venue label to store under (default: databento)
  -h, --help     print this and exit 0
  -V, --version  print the version and exit 0

environment:
  DATABENTO_API_KEY   required, from the credential store (never argv, never logged)
  VIKE_HIST_STORE     hist-store root when --store is absent";

const SPEC: CliSpec = CliSpec {
    bin: "databento_backfill",
    usage: USAGE,
    valued: &["--dataset", "--symbol", "--schema", "--start", "--end", "--store", "--venue"],
    toggles: &[],
    positionals: 0,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = SPEC.short_circuit(&args) {
        return code;
    }
    let _guards = vike_log::init(log_config("databento-backfill"));

    let (Some(dataset), Some(symbol), Some(schema), Some(start), Some(end)) = (
        arg(&args, "--dataset"),
        arg(&args, "--symbol"),
        arg(&args, "--schema"),
        arg(&args, "--start"),
        arg(&args, "--end"),
    ) else {
        eprintln!(
            "databento_backfill: --dataset, --symbol, --schema, --start and --end are all required\n\n{USAGE}"
        );
        return ExitCode::from(2);
    };
    let Some(kind) = kind_from_schema(&schema) else {
        eprintln!("unknown schema '{schema}': want trades|ohlcv-1m|ohlcv-1h|ohlcv-1d|mbp-1|mbp-10");
        return ExitCode::FAILURE;
    };
    let venue = arg(&args, "--venue").unwrap_or_else(|| "databento".to_string());
    // ONE process-environment sweep, at the root: the hist-store root and the credential chain's
    // settings-directory override comes out of it.
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let root = store_root(arg(&args, "--store").as_deref(), &env);

    // Key from the credential store (never argv/logs); mirrors how the venue bridge crates gate on
    // absent credentials — no key means we report and exit rather than making a bad request.
    let Some(api_key) = api_key(&load_workspace_secrets_from_env(&env)) else {
        eprintln!("DATABENTO_API_KEY not set (credential store or process env)");
        return ExitCode::FAILURE;
    };

    let store = match DataFusionHist::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("open store {}: {e}", root.display());
            return ExitCode::FAILURE;
        }
    };
    // An OWNED staging directory under `<project>/tmp`, removed when the guard drops at the end of
    // `main` — including on the panic path. It was a FIXED name under the system temp directory,
    // which leaked every DBN file it ever downloaded. See `vike_backfill::cli::scratch_root`.
    let tmp = match ScratchDir::create_in(&scratch_root(&env), "vike_databento") {
        Ok(d) => d,
        Err(e) => {
            eprintln!("create scratch dir: {e}");
            return ExitCode::FAILURE;
        }
    };
    match backfill(&store, &api_key, &dataset, &venue, &symbol, &kind, &start, &end, &tmp) {
        Ok(n) => {
            tracing::info!(rows = n, %symbol, %schema, %venue, "databento backfill complete");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("backfill failed: {e}");
            ExitCode::FAILURE
        }
    }
}
