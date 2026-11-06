//! Stream/URL builders, the v1 discovery query builders and JSON decode, the manifest parse.

use super::*;
use crate::vike_archive::DEFAULT_BASE;

// ---- Stream / URL builders -----------------------------------------------------------------

#[test]
fn stream_file_names_and_kinds_round_trip() {
    for s in Stream::all() {
        assert_eq!(Stream::from_kind(s.kind()), Some(s));
    }
    assert_eq!(Stream::Book.file_name(), "book_events");
    assert_eq!(Stream::Trade.file_name(), "trades");
    assert_eq!(Stream::Quote.file_name(), "l1_quotes");
    assert_eq!(Stream::from_kind(StoreKindId::Bar), None, "the archive carries no bars");
}

#[test]
fn stream_url_matches_the_hive_layout() {
    let url = stream_url(DEFAULT_BASE, "2026-07-27", Stream::Book);
    assert_eq!(
        url,
        "https://data.vike.io/archive/venue=polymarket/date=2026-07-27/book_events.parquet"
    );
    assert_eq!(manifest_url(DEFAULT_BASE), "https://data.vike.io/archive/manifest.json");
}

// ---- v1 discovery: pure URL/query builders (no network) -----------------------------------

#[test]
fn default_discovery_base_strips_the_archive_suffix() {
    assert_eq!(default_discovery_base(DEFAULT_BASE), "https://data.vike.io");
    // A base that doesn't end in `/archive` passes through unchanged rather than guessing.
    assert_eq!(default_discovery_base("https://data.vike.io/other"), "https://data.vike.io/other");
}

#[test]
fn family_stream_url_matches_the_documented_layout() {
    let url = family_stream_url(DEFAULT_BASE, "btc", "5m", "2026-07-27", Stream::Book);
    assert_eq!(
        url,
        "https://data.vike.io/archive/venue=polymarket/asset=btc/tenor=5m/date=2026-07-27/book_events.parquet"
    );
}

// ---- v1 discovery: JSON decode (pure; fixtures matching the real live-verified shapes) -----

const DATASETS_PAGE_FIXTURE: &str = r#"{
        "datasets": [
            {
                "venue": "polymarket", "layout": "flat", "date": "2026-07-27",
                "stream": "book_events", "bytes": 7402961414, "rows": null,
                "url": "https://data.vike.io/archive/venue=polymarket/date=2026-07-27/book_events.parquet"
            },
            {
                "venue": "polymarket", "layout": "family", "asset": "btc", "tenor": "5m",
                "date": "2026-07-27", "stream": "book_events", "bytes": 1477132119, "rows": 86945860,
                "url": "https://data.vike.io/archive/venue=polymarket/asset=btc/tenor=5m/date=2026-07-27/book_events.parquet"
            }
        ],
        "total": 75, "limit": 200, "offset": 0
    }"#;

#[test]
fn parse_datasets_page_decodes_flat_rows_without_asset_tenor_and_family_rows_with_them() {
    let page = parse_datasets_page(DATASETS_PAGE_FIXTURE).unwrap();
    assert_eq!(page.total, 75);
    assert_eq!(page.limit, 200);
    assert_eq!(page.offset, 0);
    assert_eq!(page.datasets.len(), 2);

    let flat = &page.datasets[0];
    assert_eq!(flat.layout, "flat");
    assert_eq!(flat.asset, None, "flat rows omit asset, not null-but-present");
    assert_eq!(flat.tenor, None);
    assert_eq!(flat.rows, None, "vendor publishes null row counts for flat rows");

    let family = &page.datasets[1];
    assert_eq!(family.layout, "family");
    assert_eq!(family.asset.as_deref(), Some("btc"));
    assert_eq!(family.tenor.as_deref(), Some("5m"));
    assert_eq!(family.bytes, 1_477_132_119);
    assert_eq!(family.rows, Some(86_945_860));
}

#[test]
fn parse_datasets_page_rejects_garbage() {
    assert!(parse_datasets_page("not json").is_err());
}

/// The exact real response shape captured live against the production archive
/// (`GET /v1/archive/url?asset=btc&tenor=5m&date=2026-07-27&stream=book_events`).
const RESOLVED_DATASET_FIXTURE: &str = r#"{
        "asset":"btc","bytes":1477132119,"date":"2026-07-27","layout":"family","rows":86945860,
        "tenor":"5m","stream":"book_events","venue":"polymarket",
        "url":"https://data.vike.io/archive/venue=polymarket/asset=btc/tenor=5m/date=2026-07-27/book_events.parquet"
    }"#;

#[test]
fn parse_dataset_decodes_the_real_resolved_shape() {
    let d = parse_dataset(RESOLVED_DATASET_FIXTURE).unwrap();
    assert_eq!(d.venue, "polymarket");
    assert_eq!(d.layout, "family");
    assert_eq!(d.asset.as_deref(), Some("btc"));
    assert_eq!(d.tenor.as_deref(), Some("5m"));
    assert_eq!(d.date, "2026-07-27");
    assert_eq!(d.stream, "book_events");
    assert_eq!(d.bytes, 1_477_132_119);
    assert_eq!(d.rows, Some(86_945_860));
    assert_eq!(
        d.url,
        "https://data.vike.io/archive/venue=polymarket/asset=btc/tenor=5m/date=2026-07-27/book_events.parquet"
    );
}

#[test]
fn parse_dataset_rejects_garbage() {
    assert!(parse_dataset("not json").is_err());
}

// ---- manifest parse (pure; a trimmed real-shape fixture) -----------------------------------

const MANIFEST_FIXTURE: &str = r#"{
        "venue": "polymarket",
        "generated_at": "2026-07-28T21:19:30Z",
        "partitioning": "venue=polymarket/date=YYYY-MM-DD/{book_events,trades,l1_quotes}.parquet",
        "schema": {"book_events": [{"name": "token_id", "type": "String"}]},
        "auth": {"scheme": "header", "header": "X-API-Key"},
        "range_requests": true,
        "free_sample": {"path": "/archive/samples/"},
        "dates": [
            {
                "date": "2026-07-27",
                "streams": {
                    "book_events": {"rows": null, "bytes": 7402961414},
                    "trades": {"rows": null, "bytes": 15490280},
                    "l1_quotes": {"rows": null, "bytes": 617280628}
                }
            },
            {
                "date": "2026-07-26",
                "streams": {
                    "book_events": {"rows": null, "bytes": 6537528677},
                    "trades": {"rows": null, "bytes": 14202938},
                    "l1_quotes": {"rows": null, "bytes": 466315902}
                }
            }
        ]
    }"#;

#[test]
fn parse_manifest_decodes_dates_and_stream_sizes_ignoring_unknown_top_level_fields() {
    let m = parse_manifest(MANIFEST_FIXTURE).unwrap();
    assert_eq!(m.venue, "polymarket");
    assert!(m.range_requests);
    assert_eq!(m.dates.len(), 2);
    assert_eq!(m.dates[0].date, "2026-07-27");
    let book = &m.dates[0].streams["book_events"];
    assert_eq!(book.bytes, 7_402_961_414);
    assert_eq!(book.rows, None, "vendor publishes null row counts today");
    assert_eq!(m.dates[1].streams["trades"].bytes, 14_202_938);
}

#[test]
fn parse_manifest_rejects_garbage() {
    assert!(parse_manifest("not json").is_err());
}
