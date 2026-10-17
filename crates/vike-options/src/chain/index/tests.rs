//! The option index's venue-free half: venue filtering, the expiry key, `missing_from` and the
//! refresh rule. Its lookups over REAL `public/get_instruments` rows run through the deribit
//! catalog parse, so they live in `crates/bridges/deribit/tests/offline/option_index_payloads.rs`.

use super::*;
use vike_marketdata::{Expiry, OptionChain, OptionQuote, StrikeRow, UnderlyingKind};

/// 2026-10-05 08:00 UTC, the fixture's `5OCT26` expiry instant.
const OCT5_MS: i64 = 1_791_187_200_000;

#[test]
fn another_venues_instrument_is_ignored() {
    let mut foreign =
        test_option("BTC-27JUN26-100000-C", "BTC", "2026-06-27", 1e5, OptionKind::Call, "BTC");
    foreign.venue = "okx".into();
    let index = OptionIndex::from_instruments([&foreign]);
    assert!(index.is_empty());
}

#[test]
fn the_expiry_key_is_the_utc_date_of_the_expiry_instant() {
    assert_eq!(expiry_date_iso(OCT5_MS).as_deref(), Some("2026-10-05"));
    assert_eq!(expiry_date_iso(crate::expiry_ms("2026-06-27")).as_deref(), Some("2026-06-27"));
}

/// A one-quote BTC grid holding `name`.
fn grid_with(name: &str) -> BTreeMap<String, UnderlyingChains> {
    let chain = OptionChain {
        underlying: "BTC".into(),
        underlying_kind: UnderlyingKind::Crypto,
        underlying_price: Some(100_000.0),
        expiry: Expiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() },
        asof_ms: 0,
        source: "deribit".into(),
        rows: vec![StrikeRow {
            strike: 100_000.0,
            call: Some(OptionQuote {
                instrument_name: Some(name.into()),
                ..OptionQuote::new(100_000.0, OptionKind::Call)
            }),
            put: None,
        }],
    };
    let mut chains = BTreeMap::new();
    chains.insert("2026-06-27".to_string(), chain);
    let mut by = BTreeMap::new();
    by.insert(
        "BTC".to_string(),
        UnderlyingChains { default_expiry: "2026-06-27".into(), expiries: Vec::new(), chains },
    );
    by
}

#[test]
fn missing_from_names_the_grid_quotes_the_index_lacks() {
    let known = "BTC-27JUN26-100000-C";
    let index = OptionIndex::from_instruments([&test_option(
        known,
        "BTC",
        "2026-06-27",
        100_000.0,
        OptionKind::Call,
        "BTC",
    )]);
    assert_eq!(index.missing_from(&grid_with(known)), None);
    assert_eq!(
        index.missing_from(&grid_with("BTC-27JUN26-105000-C")),
        Some((1, "BTC-27JUN26-105000-C".to_string()))
    );
}

#[test]
fn the_refresh_rule_retries_an_empty_index_and_refetches_a_stale_one_rarely() {
    const MS: Duration = Duration::from_millis(1);
    let empty = OptionIndex::default();
    let grid = grid_with("BTC-27JUN26-100000-C");
    assert!(index_refresh_due(&empty, &grid, None), "never tried: fetch now");
    assert!(!index_refresh_due(&empty, &grid, Some(INDEX_RETRY - MS)));
    assert!(index_refresh_due(&empty, &grid, Some(INDEX_RETRY)), "empty: retry each minute");

    let index = OptionIndex::from_instruments([&test_option(
        "BTC-27JUN26-100000-C",
        "BTC",
        "2026-06-27",
        100_000.0,
        OptionKind::Call,
        "BTC",
    )]);
    assert!(
        !index_refresh_due(&index, &grid, Some(10 * INDEX_REFRESH)),
        "every grid name indexed: never refetch"
    );
    let stale = grid_with("BTC-27JUN26-105000-C");
    assert!(!index_refresh_due(&index, &stale, Some(INDEX_REFRESH - MS)), "throttled");
    assert!(index_refresh_due(&index, &stale, Some(INDEX_REFRESH)), "a new listing: refetch");
}
