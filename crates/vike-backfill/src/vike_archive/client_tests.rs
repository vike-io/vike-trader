//! The discovery request builders and the offline fallback of `ArchiveClient::resolve_family_url`.

use super::*;
use crate::vike_archive::DEFAULT_BASE;

#[test]
fn datasets_query_is_empty_when_every_filter_is_none() {
    assert_eq!(datasets_query(&DatasetFilter::default()), "");
}

#[test]
fn datasets_query_builds_only_the_set_filters_in_order() {
    let f = DatasetFilter {
        asset: Some("btc".to_string()),
        tenor: Some("5m".to_string()),
        stream: Some(Stream::Book),
        from: Some("2026-07-26".to_string()),
        to: Some("2026-07-27".to_string()),
        limit: Some(50),
        offset: Some(10),
    };
    assert_eq!(
        datasets_query(&f),
        "?asset=btc&tenor=5m&stream=book_events&from=2026-07-26&to=2026-07-27&limit=50&offset=10"
    );
    // A partial filter only emits the fields that are actually set.
    let partial = DatasetFilter { asset: Some("eth".to_string()), ..Default::default() };
    assert_eq!(datasets_query(&partial), "?asset=eth");
}

#[test]
fn resolve_query_appends_asset_tenor_only_when_both_present() {
    assert_eq!(
        resolve_query("2026-07-27", Stream::Book, None, None),
        "?date=2026-07-27&stream=book_events"
    );
    assert_eq!(
        resolve_query("2026-07-27", Stream::Book, Some("btc"), Some("5m")),
        "?date=2026-07-27&stream=book_events&asset=btc&tenor=5m"
    );
}

/// The selection precedence [`ArchiveClient::resolve_family_url`] implements: when discovery is
/// unreachable, it MUST fall back to the exact string [`family_stream_url`] builds — never
/// panic, never silently ingest nothing. `127.0.0.1:1` is an unassigned port with nothing
/// listening, so the connect fails immediately and deterministically without touching the
/// internet — an offline-safe way to unit-test the real fallback wiring (not just the pure
/// builder function in isolation).
#[test]
fn resolve_family_url_falls_back_to_the_constructed_path_when_discovery_is_unreachable() {
    let client = ArchiveClient::new(DEFAULT_BASE, None);
    let target = FamilyTarget { discovery_base: "http://127.0.0.1:1", asset: "btc", tenor: "5m" };
    let url = client.resolve_family_url(&target, "2026-07-27", Stream::Book);
    assert_eq!(
        url,
        family_stream_url(DEFAULT_BASE, "btc", "5m", "2026-07-27", Stream::Book),
        "an unreachable discovery endpoint must fall back to the directly-constructed URL"
    );
}
