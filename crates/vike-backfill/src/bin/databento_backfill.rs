//! Databento historical backfill CLI. Fetches one schema/symbol/date-range from
//! `hist.databento.com` and ingests it into the DataFusion hist store.
//!
//! Usage:
//!   databento_backfill --dataset GLBX.MDP3 --symbol ESZ4 --schema trades \
//!       --start 2024-01-02T00:00:00 --end 2024-01-02T00:01:00 [--store DIR] [--venue NAME]
//!
//! Schema → series: trades|ohlcv-1m|ohlcv-1h|ohlcv-1d|mbp-1|mbp-10.
//! Key: DATABENTO_API_KEY from the credential store (the settings database), then the process env
//! (never argv).

use std::process::ExitCode;

use vike_backfill::cli::{log_config, parse, scoped_credentials, scratch_root, store_root};
use vike_backfill::databento::{DbnKind, backfill};
use vike_paths::scratch::ScratchDir;
use vike_secrets::{KeyScope, ScopedSecrets, UndeclaredKey};
use vike_store::DataFusionHist;

/// **Every credential name this binary declares** — owner ruling 2026-09-16, a process
/// materialises only what it asked for.
///
/// One name. A `databento_backfill` run used to hold the whole credential store in memory (67 rows
/// on the live box) to read this single API key; it now holds this row and no other.
const SCOPE: [&str; 1] = ["DATABENTO_API_KEY"];

/// The Databento key, resolved HERE in the binary (settings STEP 2 — the adapter takes it as a
/// `&str` parameter and reads no environment of its own): the CREDENTIAL STORE (the settings
/// database) — else the process environment (a CI/local override that touches no gitignored
/// file). `Ok(None)` ⇒ the live gate in `main` reports and exits. The sibling `tardis_backfill` bin reads its key the same
/// way.
///
/// `store` is the already-resolved SCOPED credential answer, built once in [`main`] from the SAME
/// `std::env::vars()` sweep that resolves the hist-store root — so this bin performs one
/// environment sweep, not two.
///
/// # ⚠ Why this returns a `Result` where it used to return an `Option`
///
/// Absent credentials are this workspace's LIVE GATE: a name that is not in the store is a silent,
/// correct "not configured". A name this binary forgot to DECLARE would look identical through an
/// `Option` — a silent outage wearing the costume of a correct default — so
/// `vike_secrets::Lookup` answers three states and the undeclared one is an `Err` that `main`
/// reports and exits on. See that type's doc.
///
/// # Errors
/// [`UndeclaredKey`] when the name asked for is outside [`SCOPE`].
fn api_key(store: &ScopedSecrets) -> Result<Option<String>, UndeclaredKey> {
    Ok(store
        .get("DATABENTO_API_KEY")
        .declared()?
        .map(str::to_string)
        .or_else(|| std::env::var("DATABENTO_API_KEY").ok()))
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

/// Fetch history from the Databento `timeseries.get_range` CSV API and ingest it into the DataFusion
/// hist store. Idempotent per window by a `databento:{kind}:{symbol}:{window}` commit key.
#[derive(clap::Parser, Debug)]
#[command(after_help = "environment:\n  DATABENTO_API_KEY   required, from the credential store \
                        (never argv, never logged)")]
struct Args {
    /// Databento dataset code, e.g. GLBX.MDP3
    #[arg(long)]
    dataset: String,
    /// The instrument symbol within that dataset
    #[arg(long)]
    symbol: String,
    /// trades | ohlcv-1m | ohlcv-1h | ohlcv-1d | mbp-1 | mbp-10
    #[arg(long)]
    schema: String,
    /// Window start (Databento timestamp form)
    #[arg(long)]
    start: String,
    /// Window end
    #[arg(long)]
    end: String,
    /// Hist-store root (default: <repo>/market_data/hist, else the project's own)
    #[arg(long)]
    store: Option<String>,
    /// Venue label to store under
    #[arg(long, default_value = "databento")]
    venue: String,
}

fn main() -> ExitCode {
    // ⚠ FIRST — `--help`, `--version` and a usage error answer before any log file, store or call.
    let Args { dataset, symbol, schema, start, end, store, venue } = parse("databento_backfill");
    // ONE process-environment sweep, at the root: the log level's settings directory, the
    // hist-store root and the credential chain's settings-directory override all come out of it.
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let _guards = vike_log::init(log_config("databento-backfill", &env));

    let Some(kind) = kind_from_schema(&schema) else {
        eprintln!("unknown schema '{schema}': want trades|ohlcv-1m|ohlcv-1h|ohlcv-1d|mbp-1|mbp-10");
        return ExitCode::FAILURE;
    };
    let root = store_root(store.as_deref(), &env);

    // Key from the credential store (never argv/logs); mirrors how the venue bridge crates gate on
    // absent credentials — no key means we report and exit rather than making a bad request.
    let scoped = match scoped_credentials(&env, &KeyScope::of(SCOPE)) {
        Ok(scoped) => scoped,
        Err(e) => {
            eprintln!("databento_backfill: {e}");
            return ExitCode::FAILURE;
        }
    };
    let api_key = match api_key(&scoped) {
        // A name outside `SCOPE` is a DEFECT IN THIS BINARY, not an unconfigured box — so it is
        // reported as one and never degrades into the live gate's silence.
        Err(undeclared) => {
            eprintln!("credential scope defect: {undeclared}");
            return ExitCode::FAILURE;
        }
        Ok(None) => {
            eprintln!("DATABENTO_API_KEY not set (credential store or process env)");
            return ExitCode::FAILURE;
        }
        Ok(Some(key)) => key,
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
