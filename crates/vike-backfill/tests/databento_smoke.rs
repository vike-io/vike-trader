//! Live Databento smoke — #[ignore]d and self-skipping. Fetches a 1-minute trades slice for a
//! single symbol into a temp store and asserts a non-empty, well-formed ingest. Requires
//! DATABENTO_API_KEY in the workspace .env; keep the range tiny to keep cost ≈ $0.
//!
//! Run: cargo test -p vike-backfill --features databento --test databento_smoke -- --ignored --nocapture

#![cfg(feature = "databento")]

use vike_backfill::databento::{backfill, DbnKind};
use vike_bridge_core::credentials::load_workspace_dotenv_from;
use vike_data::DataFusionHist;

#[test]
#[ignore = "live: needs DATABENTO_API_KEY, spends a tiny amount"]
fn databento_trades_one_minute_roundtrip() {
    // The CALLER resolves the key (settings STEP 2 — the adapter takes it as a `&str` parameter
    // and reads no env of its own), exactly as the `databento_backfill` bin and `tardis_smoke` do.
    // Same two-source precedence the bin uses, so a run with the key exported (rather than in the
    // gitignored `.env`) still self-gates the way it did before the read moved out of the adapter.
    let dotenv = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let key = dotenv
        .get("DATABENTO_API_KEY")
        .cloned()
        .or_else(|| std::env::var("DATABENTO_API_KEY").ok());
    let Some(api_key) = key else {
        eprintln!("skip: DATABENTO_API_KEY not set");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(tmp.path().join("hist")).unwrap();
    let n = backfill(
        &store,
        &api_key,
        "GLBX.MDP3",
        "databento",
        "ESZ4",
        &DbnKind::Trades,
        "2024-06-03T14:30:00",
        "2024-06-03T14:31:00",
        &tmp.path().join("tmp"),
    )
    .expect("backfill");
    assert!(n > 0, "expected trades in the 1-minute window");
}
