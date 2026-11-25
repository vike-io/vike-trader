//! Live Tardis smoke — #[ignore]d. Ingests one day of `trades` for a single symbol into a temp
//! store and asserts a non-empty ingest. Uses the FREE first-of-month sample when TARDIS_API_KEY
//! is absent, so it can run keyless (still #[ignore]d — it hits the network).
//!
//! Run: cargo test -p vike-backfill --features tardis --test integration -- --ignored --nocapture tardis_smoke::

#![cfg(feature = "tardis")]

use vike_backfill::tardis::{TardisKind, backfill_range};
use vike_store::HistoryStoreDataFusion;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlType, VenueUrls};

#[test]
#[ignore = "live: hits datasets.tardis.dev (free first-of-month sample)"]
fn tardis_trades_one_day_roundtrip() {
    let secrets = vike_secrets::smoke_vars();
    let api_key = secrets.get("TARDIS_API_KEY").cloned();
    let seed = VenueUrls::seed("tardis");
    let base = seed.url(UrlType::Live, UrlChannel::History, UrlProtocol::Rest, "").unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(tmp.path().join("hist")).unwrap();
    // First of the month = free sample even without a key.
    let n = backfill_range(
        &store,
        base,
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
