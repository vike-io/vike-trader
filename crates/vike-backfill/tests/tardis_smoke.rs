//! Live Tardis smoke — #[ignore]d. Ingests one day of `trades` for a single symbol into a temp
//! store and asserts a non-empty ingest. Uses the FREE first-of-month sample when TARDIS_API_KEY
//! is absent, so it can run keyless (still #[ignore]d — it hits the network).
//!
//! Run: cargo test -p vike-backfill --features tardis --test tardis_smoke -- --ignored --nocapture

#![cfg(feature = "tardis")]

use vike_backfill::tardis::{TardisKind, backfill_range};
use vike_bridge_core::credentials::load_workspace_dotenv_from;
use vike_data::DataFusionHist;

#[test]
#[ignore = "live: hits datasets.tardis.dev (free first-of-month sample)"]
fn tardis_trades_one_day_roundtrip() {
    let dotenv = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let api_key = dotenv.get("TARDIS_API_KEY").cloned();
    let tmp = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(tmp.path().join("hist")).unwrap();
    // First of the month = free sample even without a key.
    let n = backfill_range(
        &store,
        api_key.as_deref(),
        "deribit",
        "tardis",
        "BTC-PERPETUAL",
        &TardisKind::Trades,
        &[(2024, 3, 1)],
    )
    .expect("backfill");
    assert!(n > 0, "expected trades on 2024-03-01");
}
