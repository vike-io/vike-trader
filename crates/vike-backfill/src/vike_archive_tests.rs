use super::*;
use datafusion::arrow::array::{Array, ArrayRef};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::parquet::arrow::ArrowWriter;
use datafusion::parquet::file::properties::WriterProperties;
use std::assert_matches;
use vike_data::{BulkConfig, TsRange};

// ---- Stream / URL builders -----------------------------------------------------------------

#[test]
fn stream_file_names_and_kinds_round_trip() {
    for s in Stream::all() {
        assert_eq!(Stream::from_kind(s.kind()), Some(s));
    }
    assert_eq!(Stream::Book.file_name(), "book_events");
    assert_eq!(Stream::Trade.file_name(), "trades");
    assert_eq!(Stream::Quote.file_name(), "l1_quotes");
    assert_eq!(Stream::from_kind("garbage"), None);
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

// ---- level JSON decode ----------------------------------------------------------------------

#[test]
fn parse_levels_json_handles_numbers_empty_and_garbage() {
    assert_eq!(parse_levels_json(""), Vec::<BookLevel>::new());
    assert_eq!(
        parse_levels_json("[[0.5,100.0],[0.49,50.0]]"),
        vec![BookLevel::new(0.5, 100.0), BookLevel::new(0.49, 50.0)]
    );
    assert_eq!(parse_levels_json("not json"), Vec::<BookLevel>::new());
}

// ---- decimal scale decode ---------------------------------------------------------------------

#[test]
fn decimal_scales_decode_price_and_size() {
    let arr = Decimal128Array::from(vec![1390_i128, 5_208_325_i128])
        .with_precision_and_scale(18, 6)
        .unwrap();
    assert!((dec(&arr, 0, PRICE_SCALE_DIVISOR) - 0.139).abs() < 1e-12);
    assert!((dec(&arr, 1, SIZE_SCALE_DIVISOR) - 5.208325).abs() < 1e-12);
}

// ---- book_events -> BookUpdate (synthetic RecordBatch, no Parquet round trip) --------------

type BookRow<'a> =
    (&'a str, i64, i64, u64, &'a str, &'a str, f64, f64, &'a str, &'a str, f64, &'a str);

fn book_events_batch(rows: &[BookRow<'_>]) -> RecordBatch {
    let schema = Schema::new(vec![
        Field::new("token_id", DataType::Utf8, false),
        Field::new("ts", DataType::Int64, false),
        Field::new("local_ts", DataType::Int64, false),
        Field::new("seq", DataType::UInt64, false),
        Field::new("event_type", DataType::Binary, false),
        Field::new("side", DataType::Binary, false),
        Field::new("price", DataType::Decimal128(9, 4), false),
        Field::new("size", DataType::Decimal128(18, 6), false),
        Field::new("bids", DataType::Utf8, false),
        Field::new("asks", DataType::Utf8, false),
        Field::new("tick_size", DataType::Decimal128(9, 4), false),
        Field::new("status", DataType::Utf8, false),
    ]);
    let price = Decimal128Array::from(
        rows.iter().map(|r| (r.6 * 10_000.0).round() as i128).collect::<Vec<_>>(),
    )
    .with_precision_and_scale(9, 4)
    .unwrap();
    let size = Decimal128Array::from(
        rows.iter().map(|r| (r.7 * 1_000_000.0).round() as i128).collect::<Vec<_>>(),
    )
    .with_precision_and_scale(18, 6)
    .unwrap();
    let tick_size = Decimal128Array::from(
        rows.iter().map(|r| (r.10 * 10_000.0).round() as i128).collect::<Vec<_>>(),
    )
    .with_precision_and_scale(9, 4)
    .unwrap();
    RecordBatch::try_new(
        Arc::new(schema),
        vec![
            Arc::new(StringArray::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
            Arc::new(Int64Array::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
            Arc::new(Int64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
            Arc::new(UInt64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>())),
            Arc::new(BinaryArray::from(rows.iter().map(|r| r.4.as_bytes()).collect::<Vec<_>>())),
            Arc::new(BinaryArray::from(rows.iter().map(|r| r.5.as_bytes()).collect::<Vec<_>>())),
            Arc::new(price) as ArrayRef,
            Arc::new(size) as ArrayRef,
            Arc::new(StringArray::from(rows.iter().map(|r| r.8).collect::<Vec<_>>())),
            Arc::new(StringArray::from(rows.iter().map(|r| r.9).collect::<Vec<_>>())),
            Arc::new(tick_size) as ArrayRef,
            Arc::new(StringArray::from(rows.iter().map(|r| r.11).collect::<Vec<_>>())),
        ],
    )
    .unwrap()
}

/// The FAMILY-layout twin of [`book_events_batch`]: identical row shape, but `event_type`/
/// `side` are `Utf8` instead of `Binary` — the real, live-verified physical-type divergence
/// between the flat and family `book_events` exports (see [`StrOrBinCol`]'s doc). Proves
/// [`book_updates_from_batch`] decodes the family layout's encoding too, not just the flat one.
fn book_events_batch_family_utf8(rows: &[BookRow<'_>]) -> RecordBatch {
    let schema = Schema::new(vec![
        Field::new("token_id", DataType::Utf8, false),
        Field::new("ts", DataType::Int64, false),
        Field::new("local_ts", DataType::Int64, false),
        Field::new("seq", DataType::UInt64, false),
        Field::new("event_type", DataType::Utf8, false),
        Field::new("side", DataType::Utf8, false),
        Field::new("price", DataType::Decimal128(9, 4), false),
        Field::new("size", DataType::Decimal128(18, 6), false),
        Field::new("bids", DataType::Utf8, false),
        Field::new("asks", DataType::Utf8, false),
        Field::new("tick_size", DataType::Decimal128(9, 4), false),
        Field::new("status", DataType::Utf8, false),
    ]);
    let price = Decimal128Array::from(
        rows.iter().map(|r| (r.6 * 10_000.0).round() as i128).collect::<Vec<_>>(),
    )
    .with_precision_and_scale(9, 4)
    .unwrap();
    let size = Decimal128Array::from(
        rows.iter().map(|r| (r.7 * 1_000_000.0).round() as i128).collect::<Vec<_>>(),
    )
    .with_precision_and_scale(18, 6)
    .unwrap();
    let tick_size = Decimal128Array::from(
        rows.iter().map(|r| (r.10 * 10_000.0).round() as i128).collect::<Vec<_>>(),
    )
    .with_precision_and_scale(9, 4)
    .unwrap();
    RecordBatch::try_new(
        Arc::new(schema),
        vec![
            Arc::new(StringArray::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
            Arc::new(Int64Array::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
            Arc::new(Int64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
            Arc::new(UInt64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>())),
            Arc::new(StringArray::from(rows.iter().map(|r| r.4).collect::<Vec<_>>())),
            Arc::new(StringArray::from(rows.iter().map(|r| r.5).collect::<Vec<_>>())),
            Arc::new(price) as ArrayRef,
            Arc::new(size) as ArrayRef,
            Arc::new(StringArray::from(rows.iter().map(|r| r.8).collect::<Vec<_>>())),
            Arc::new(StringArray::from(rows.iter().map(|r| r.9).collect::<Vec<_>>())),
            Arc::new(tick_size) as ArrayRef,
            Arc::new(StringArray::from(rows.iter().map(|r| r.11).collect::<Vec<_>>())),
        ],
    )
    .unwrap()
}

#[test]
fn family_layouts_utf8_event_type_and_side_decode_identically_to_flats_binary_encoding() {
    let rows: Vec<BookRow<'_>> = vec![
        (
            "TOKA",
            1_700_000_000_000,
            1_700_000_000_003,
            1,
            "book",
            "none",
            0.0,
            0.0,
            "[[0.5,100.0]]",
            "[[0.51,80.0]]",
            0.01,
            "",
        ),
        ("TOKA", 1, 2, 5, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
        ("TOKA", 1, 2, 6, "price_change", "sell", 0.60, 0.0, "", "", 0.01, ""),
        ("TOKA", 9, 10, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "gap_start"),
    ];
    let flat = book_updates_from_batch(&book_events_batch(&rows)).unwrap();
    let family = book_updates_from_batch(&book_events_batch_family_utf8(&rows)).unwrap();
    // `BookUpdate` derives no `PartialEq` (vike-model), so compare via `Debug` — still a
    // precise structural comparison, just not the `==` operator.
    assert_eq!(
        format!("{flat:?}"),
        format!("{family:?}"),
        "the same logical rows must decode identically regardless of layout"
    );
    assert_eq!(flat.len(), 4);
}

#[test]
fn snapshot_row_decodes_full_depth_and_per_row_symbol() {
    let b = book_events_batch(&[(
        "TOKA",
        1_700_000_000_000,
        1_700_000_000_003,
        1,
        "book",
        "none",
        0.0,
        0.0,
        "[[0.5,100.0]]",
        "[[0.51,80.0]]",
        0.01,
        "",
    )]);
    let out = book_updates_from_batch(&b).unwrap();
    assert_eq!(out.len(), 1);
    let u = &out[0];
    assert_eq!(u.kind, BookUpdateKind::Snapshot);
    assert_eq!(u.ts, 1_700_000_000_000);
    assert_eq!(u.local_ts, 1_700_000_000_003);
    assert_eq!(u.seq, 1);
    assert_eq!(u.bids, vec![BookLevel::new(0.5, 100.0)]);
    assert_eq!(u.asks, vec![BookLevel::new(0.51, 80.0)]);
    assert!((u.tick_size - 0.01).abs() < 1e-12);
    assert_eq!(u.symbol, "TOKA");
}

#[test]
fn delta_row_decodes_the_populated_side_from_decimal_columns() {
    let b = book_events_batch(&[
        ("TOKA", 1, 2, 5, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
        ("TOKA", 1, 2, 6, "price_change", "sell", 0.60, 0.0, "", "", 0.01, ""),
    ]);
    let out = book_updates_from_batch(&b).unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].kind, BookUpdateKind::Delta);
    assert!((out[0].bids[0].price - 0.42).abs() < 1e-9);
    assert!((out[0].bids[0].qty - 7.0).abs() < 1e-9);
    assert!(out[0].asks.is_empty());
    assert!(out[1].bids.is_empty());
    assert!((out[1].asks[0].price - 0.60).abs() < 1e-9);
}

#[test]
fn status_rows_map_to_the_right_kind_with_empty_levels() {
    let b = book_events_batch(&[
        ("TOKA", 9, 10, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "gap_start"),
        ("TOKA", 9, 11, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "stale"),
        ("TOKA", 9, 12, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "live_resume"),
    ]);
    let out = book_updates_from_batch(&b).unwrap();
    assert_eq!(out.len(), 3);
    assert_eq!(out[0].kind, BookUpdateKind::GapStart);
    assert_eq!(out[1].kind, BookUpdateKind::Stale);
    assert_eq!(out[2].kind, BookUpdateKind::LiveResume);
    assert!(out.iter().all(|u| u.bids.is_empty() && u.asks.is_empty()));
}

#[test]
fn trade_and_tick_size_change_rows_are_skipped() {
    let b = book_events_batch(&[
        ("TOKA", 1, 2, 0, "trade", "sell", 0.5, 1.0, "", "", 0.0, ""),
        ("TOKA", 1, 2, 0, "tick_size_change", "none", 0.0, 0.0, "", "", 0.02, ""),
    ]);
    let out = book_updates_from_batch(&b).unwrap();
    assert!(out.is_empty(), "trade/tick_size_change carry no BookUpdate: {out:?}");
}

#[test]
fn mixed_tokens_in_one_batch_keep_their_own_symbol() {
    let b = book_events_batch(&[
        ("TOKA", 1, 1, 0, "book", "none", 0.0, 0.0, "[]", "[]", 0.01, ""),
        ("TOKB", 2, 2, 0, "book", "none", 0.0, 0.0, "[]", "[]", 0.01, ""),
    ]);
    let out = book_updates_from_batch(&b).unwrap();
    assert_eq!(out[0].symbol, "TOKA");
    assert_eq!(out[1].symbol, "TOKB");
}

// ---- trades -> TradeTick ----------------------------------------------------------------------

fn trades_batch(rows: &[(&str, i64, i64, f64, f64, &str)]) -> RecordBatch {
    let schema = Schema::new(vec![
        Field::new("token_id", DataType::Utf8, false),
        Field::new("ts", DataType::Int64, false),
        Field::new("local_ts", DataType::Int64, false),
        Field::new("price", DataType::Float64, false),
        Field::new("size", DataType::Float64, false),
        Field::new("side", DataType::Utf8, false),
    ]);
    RecordBatch::try_new(
        Arc::new(schema),
        vec![
            Arc::new(StringArray::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
            Arc::new(Int64Array::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
            Arc::new(Int64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
            Arc::new(Float64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>())),
            Arc::new(Float64Array::from(rows.iter().map(|r| r.4).collect::<Vec<_>>())),
            Arc::new(StringArray::from(rows.iter().map(|r| r.5).collect::<Vec<_>>())),
        ],
    )
    .unwrap()
}

#[test]
fn trades_from_batch_inverts_the_taker_side_convention() {
    let b = trades_batch(&[
        ("TOKA", 1_700_000_000_100, 1_700_000_000_101, 0.95, 3.0, "sell"),
        ("TOKA", 1_700_000_000_200, 1_700_000_000_201, 0.10, 1.0, "buy"),
    ]);
    let out = trades_from_batch(&b).unwrap();
    assert_eq!(out.len(), 2);
    assert!(out[0].is_buyer_maker, "side=sell -> taker sold -> is_buyer_maker=true");
    assert_eq!(out[0].price, 0.95);
    assert_eq!(out[0].size, 3.0);
    assert_eq!(out[0].symbol, "TOKA");
    assert!(!out[1].is_buyer_maker, "side=buy -> taker bought -> is_buyer_maker=false");
}

// ---- l1_quotes -> QuoteTick -------------------------------------------------------------------

#[test]
fn quotes_from_batch_decodes_decimal_l1() {
    let schema = Schema::new(vec![
        Field::new("token_id", DataType::Utf8, false),
        Field::new("ts", DataType::Int64, false),
        Field::new("local_ts", DataType::Int64, false),
        Field::new("bid", DataType::Decimal128(9, 4), false),
        Field::new("ask", DataType::Decimal128(9, 4), false),
        Field::new("bid_size", DataType::Decimal128(18, 6), false),
        Field::new("ask_size", DataType::Decimal128(18, 6), false),
    ]);
    let bid = Decimal128Array::from(vec![4_400_i128]).with_precision_and_scale(9, 4).unwrap();
    let ask = Decimal128Array::from(vec![4_700_i128]).with_precision_and_scale(9, 4).unwrap();
    let bid_size =
        Decimal128Array::from(vec![10_000_000_i128]).with_precision_and_scale(18, 6).unwrap();
    let ask_size =
        Decimal128Array::from(vec![8_000_000_i128]).with_precision_and_scale(18, 6).unwrap();
    let b = RecordBatch::try_new(
        Arc::new(schema),
        vec![
            Arc::new(StringArray::from(vec!["TOKA"])),
            Arc::new(Int64Array::from(vec![1_700_000_000_200_i64])),
            Arc::new(Int64Array::from(vec![1_700_000_000_201_i64])),
            Arc::new(bid) as ArrayRef,
            Arc::new(ask) as ArrayRef,
            Arc::new(bid_size) as ArrayRef,
            Arc::new(ask_size) as ArrayRef,
        ],
    )
    .unwrap();
    let out = quotes_from_batch(&b).unwrap();
    assert_eq!(out.len(), 1);
    assert!((out[0].bid - 0.44).abs() < 1e-9);
    assert!((out[0].ask - 0.47).abs() < 1e-9);
    assert!((out[0].bid_size - 10.0).abs() < 1e-9);
    assert!((out[0].ask_size - 8.0).abs() < 1e-9);
    assert_eq!(out[0].symbol, "TOKA");
}

// ---- row-group pruning: build a REAL multi-row-group Parquet file, decode metadata back ------

/// Writes `token_ids` (one row per id, one row group per row via `set_max_row_group_size(1)`)
/// as a minimal single-column-plus-companions Parquet file entirely in memory, and returns the
/// bytes — the "build a tiny RecordBatch, write to bytes, decode back" fixture the design brief
/// asked for, exercising the REAL Parquet writer's row-group statistics (not hand-built stats).
fn write_single_column_parquet(token_ids: &[&str]) -> Bytes {
    let schema = Arc::new(Schema::new(vec![Field::new("token_id", DataType::Utf8, false)]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(StringArray::from(token_ids.to_vec())) as ArrayRef],
    )
    .unwrap();
    let props = WriterProperties::builder().set_max_row_group_row_count(Some(1)).build();
    let mut buf = Vec::new();
    {
        let mut writer = ArrowWriter::try_new(&mut buf, schema, Some(props)).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
    }
    Bytes::from(buf)
}

#[test]
fn select_row_groups_prunes_to_the_matching_groups_only() {
    // Distinct, sorted token ids -> one row group per id (max_row_group_size=1), each group's
    // min==max==that id — an unambiguous test of the byte-range containment check.
    let ids = ["TOK_A", "TOK_B", "TOK_C", "TOK_D"];
    let bytes = write_single_column_parquet(&ids);
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes).unwrap();
    let md = builder.metadata();
    assert_eq!(md.num_row_groups(), 4, "one row group per row (max_row_group_size=1)");

    let mut want = HashSet::new();
    want.insert("TOK_B".to_string());
    let selected = select_row_groups(md, Some(&want));
    assert_eq!(selected, vec![1], "only TOK_B's own row group is selected");

    // No filter -> every row group.
    assert_eq!(select_row_groups(md, None), vec![0, 1, 2, 3]);

    // A token absent from the file -> no row group matches (assuming it sorts outside every
    // group's [min,max] — true here since "TOK_Z" > every group's max byte-wise).
    let mut absent = HashSet::new();
    absent.insert("TOK_Z".to_string());
    assert!(select_row_groups(md, Some(&absent)).is_empty());

    // Empty filter set -> nothing selected (never "everything").
    assert!(select_row_groups(md, Some(&HashSet::new())).is_empty());
}

#[test]
fn select_row_groups_reads_end_to_end_through_the_real_reader() {
    // Full round trip: write -> build reader with ONLY the pruned row groups -> decode -> the
    // resulting rows are exactly (and only) the requested token's.
    let ids = ["TOK_A", "TOK_B", "TOK_C"];
    let bytes = write_single_column_parquet(&ids);
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes).unwrap();
    let mut want = HashSet::new();
    want.insert("TOK_C".to_string());
    let selected = select_row_groups(builder.metadata(), Some(&want));
    let reader = builder.with_row_groups(selected).build().unwrap();
    let mut seen = Vec::new();
    for batch in reader {
        let batch = batch.unwrap();
        let col = batch.column(0).as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..batch.num_rows() {
            seen.push(col.value(i).to_string());
        }
    }
    assert_eq!(seen, vec!["TOK_C".to_string()]);
}

// ---- streaming ingest: memory-bounded per-row-group flush (the defect this PR fixes) --------

/// Writes `rows` as a `book_events`-shaped Parquet file (the same 12-column schema
/// [`book_events_batch`] builds), forced to split into row groups of at most
/// `max_rows_per_group` rows each (`ArrowWriter` auto-splits a single `write()` call across
/// row-group boundaries once `WriterProperties::set_max_row_group_row_count` is set — the same
/// mechanism [`write_single_column_parquet`] already relies on above).
fn write_book_events_parquet(rows: &[BookRow<'_>], max_rows_per_group: usize) -> Bytes {
    let batch = book_events_batch(rows);
    let schema = batch.schema();
    let props =
        WriterProperties::builder().set_max_row_group_row_count(Some(max_rows_per_group)).build();
    let mut buf = Vec::new();
    {
        let mut writer = ArrowWriter::try_new(&mut buf, schema, Some(props)).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
    }
    Bytes::from(buf)
}

/// The headline proof for this PR: [`ingest_stream_over`] flushes ONE row group at a time — not
/// after draining the whole file. Three distinct tokens, one row group each
/// (`max_rows_per_group=1`): the progress callback must fire exactly once per row group, in
/// order, with a running cumulative count — and, load-bearing, EACH row group's data must
/// already be durable in the store by the time its own callback fires (checked with a real
/// `HistStore::scan_book_updates` from INSIDE the callback) — proving the write happens before
/// the next row group is ever opened, not batched at the end. This is the structural substitute
/// for an in-process RSS assertion (impractical here); the actual peak-RSS measurement lives in
/// the PR's live the CI box proof, not in this unit test.
#[test]
fn ingest_stream_over_flushes_each_row_group_before_moving_to_the_next() {
    let rows: Vec<BookRow<'_>> = vec![
        ("TOK_A", 10, 10, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
        ("TOK_B", 20, 20, 1, "book", "none", 0.0, 0.0, "[[0.4,20.0]]", "[[0.41,20.0]]", 0.01, ""),
        ("TOK_C", 30, 30, 1, "book", "none", 0.0, 0.0, "[[0.3,30.0]]", "[[0.31,30.0]]", 0.01, ""),
    ];
    let bytes = write_book_events_parquet(&rows, 1);
    let probe = ParquetRecordBatchReaderBuilder::try_new(bytes.clone()).unwrap();
    assert_eq!(probe.metadata().num_row_groups(), 3, "one row group per row (limit=1)");

    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let expected_symbol = ["TOK_A", "TOK_B", "TOK_C"];

    let mut seen = Vec::new();
    let total = ingest_stream_over(bytes, &store, "2026-07-27", Stream::Book, None, |p| {
        // At the moment THIS callback runs, the row group just processed must already be on
        // disk — scanning here (not after the whole run) is what makes this an incremental,
        // not an end-of-run, assertion.
        let sym = expected_symbol[p.index];
        let scanned = store.scan_book_updates(VENUE, sym, TsRange::all()).unwrap();
        assert_eq!(
            scanned.len(),
            1,
            "row group {} ({sym})'s row must already be durable when its own callback fires",
            p.index
        );
        seen.push((p.index, p.total, p.rows_this_group, p.rows_cumulative));
    })
    .unwrap();

    assert_eq!(total, 3);
    assert_eq!(
        seen,
        vec![(0, 3, 1, 1), (1, 3, 1, 2), (2, 3, 1, 3)],
        "one callback per row group, in order, with a running cumulative total"
    );
}

/// The correctness proof for the per-row-group commit-key redesign: TWO rows for the SAME
/// token, forced into TWO separate row groups. A single date-wide commit key (the pre-fix
/// scheme) would make the SECOND row's write a silent no-op — `HistStore`'s `commit_rows`
/// treats a repeated commit key as "already committed" and returns `Ok(0)` without writing
/// anything (see `vike-data`'s `datafusion_hist.rs`) — so a naive per-row-group split with the
/// OLD shared key would have silently dropped every row after a symbol's first row group. This
/// asserts both rows survive.
#[test]
fn ingest_stream_over_keeps_both_rows_when_one_symbol_spans_two_row_groups() {
    let rows: Vec<BookRow<'_>> = vec![
        ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
        ("TOK_A", 2, 2, 2, "book", "none", 0.0, 0.0, "[[0.6,11.0]]", "[[0.61,11.0]]", 0.01, ""),
    ];
    let bytes = write_book_events_parquet(&rows, 1);
    let probe = ParquetRecordBatchReaderBuilder::try_new(bytes.clone()).unwrap();
    assert_eq!(
        probe.metadata().num_row_groups(),
        2,
        "max_rows_per_group=1 over 2 rows -> 2 groups"
    );

    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let mut calls = 0usize;
    let total = ingest_stream_over(bytes, &store, "2026-07-27", Stream::Book, None, |_p| {
        calls += 1;
    })
    .unwrap();

    assert_eq!(calls, 2, "one callback per row group");
    assert_eq!(total, 2, "both rows written — neither silently dropped by a shared commit key");
    let scanned = store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap();
    assert_eq!(
        scanned.len(),
        2,
        "both row groups' rows for TOK_A must be present, not just the first"
    );
}

/// A token filter still prunes row groups (and drops unrelated rows within a kept group) when
/// ingesting through the new streaming path — the filtering behavior itself is unchanged,
/// only WHEN writes happen changed.
#[test]
fn ingest_stream_over_respects_a_token_filter() {
    let rows: Vec<BookRow<'_>> = vec![
        ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
        ("TOK_B", 2, 2, 1, "book", "none", 0.0, 0.0, "[[0.4,20.0]]", "[[0.41,20.0]]", 0.01, ""),
    ];
    let bytes = write_book_events_parquet(&rows, 1);
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let mut want = HashSet::new();
    want.insert("TOK_B".to_string());

    let total =
        ingest_stream_over(bytes, &store, "2026-07-27", Stream::Book, Some(&want), |_| {}).unwrap();
    assert_eq!(total, 1, "only TOK_B's row is written");
    assert!(store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap().is_empty());
    assert_eq!(store.scan_book_updates(VENUE, "TOK_B", TsRange::all()).unwrap().len(), 1);
}

// ---- local-file ChunkReader: proves the reader is the ONLY difference from the URL path -----

/// Opening a path that doesn't exist reports a `CollectError`, not a panic — same "one bad
/// file must not crash the whole run" posture the rest of this module holds elsewhere.
#[test]
fn local_file_reader_open_reports_a_missing_path() {
    let path = std::env::temp_dir().join("vike_archive_no_such_local_file.parquet");
    assert!(!path.exists());
    assert!(LocalFileReader::open(&path).is_err());
}

/// The headline correctness proof for the `--file` feature: the SAME Parquet bytes ingested
/// once through [`LocalFileReader`] (the new local path) and once through a plain `Bytes`
/// reader (standing in for [`HttpRangeReader`] — both are nothing more than `ChunkReader`
/// impls feeding the identical [`ingest_stream_over`] pipeline, and `Bytes` is exactly the
/// reader type every other streaming test in this module already exercises that pipeline
/// with) must decode into byte-identical mapped rows, proving the reader swap is the ONLY
/// difference between the two ingest paths — nothing about decode/mapping/commit-keying
/// changed.
#[test]
fn local_file_reader_and_a_bytes_reader_ingest_identical_rows() {
    let rows: Vec<BookRow<'_>> = vec![
        ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
        ("TOK_A", 2, 2, 2, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
        ("TOK_B", 3, 3, 1, "book", "none", 0.0, 0.0, "[[0.4,20.0]]", "[[0.41,20.0]]", 0.01, ""),
    ];
    let bytes = write_book_events_parquet(&rows, 1);

    // "URL-equivalent" path: ingest straight from the in-memory `Bytes` reader.
    let url_dir = tempfile::tempdir().unwrap();
    let url_store = DataFusionHist::open(url_dir.path()).unwrap();
    let url_total =
        ingest_stream_over(bytes.clone(), &url_store, "2026-07-27", Stream::Book, None, |_| {})
            .unwrap();

    // Local-file path: the same bytes, written to a real file on disk, opened through
    // `LocalFileReader` — the only thing that differs from the block above.
    let file_dir = tempfile::tempdir().unwrap();
    let file_path = file_dir.path().join("book_events.parquet");
    std::fs::write(&file_path, &bytes).unwrap();
    let local_store_dir = tempfile::tempdir().unwrap();
    let local_store = DataFusionHist::open(local_store_dir.path()).unwrap();
    let local_reader = LocalFileReader::open(&file_path).unwrap();
    assert_eq!(local_reader.len(), bytes.len() as u64, "LocalFileReader sees the real file size");
    let local_total =
        ingest_stream_over(local_reader, &local_store, "2026-07-27", Stream::Book, None, |_| {})
            .unwrap();

    assert_eq!(url_total, local_total, "both readers decode the same row count");
    assert_eq!(url_total, 3);

    for sym in ["TOK_A", "TOK_B"] {
        let from_url = url_store.scan_book_updates(VENUE, sym, TsRange::all()).unwrap();
        let from_local = local_store.scan_book_updates(VENUE, sym, TsRange::all()).unwrap();
        assert_eq!(
            from_url.len(),
            from_local.len(),
            "same row count for {sym} regardless of reader"
        );
        for (a, b) in from_url.iter().zip(from_local.iter()) {
            assert_eq!(a.ts, b.ts, "{sym}: ts must match");
            assert_eq!(a.local_ts, b.local_ts, "{sym}: local_ts must match");
            assert_eq!(a.seq, b.seq, "{sym}: seq must match");
            assert_eq!(a.kind, b.kind, "{sym}: kind must match");
            assert_eq!(a.tick_size, b.tick_size, "{sym}: tick_size must match");
            assert_eq!(a.bids, b.bids, "{sym}: bids must match");
            assert_eq!(a.asks, b.asks, "{sym}: asks must match");
            assert_eq!(a.symbol, b.symbol, "{sym}: symbol must match");
        }
    }
}

/// The bulk (`--file --bulk`) local-ingest twin of the test above: staging through
/// [`ingest_local_file_bulk`]/[`BulkIngestSession`] must land the SAME rows as the plain
/// (non-bulk) local-file path — the bulk profile only changes WHEN a commit happens, never
/// WHAT gets committed.
#[test]
fn ingest_local_file_bulk_matches_the_plain_local_file_path() {
    let rows: Vec<BookRow<'_>> = vec![
        ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
        ("TOK_A", 2, 2, 2, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
    ];
    let bytes = write_book_events_parquet(&rows, 1);
    let file_dir = tempfile::tempdir().unwrap();
    let file_path = file_dir.path().join("book_events.parquet");
    std::fs::write(&file_path, &bytes).unwrap();

    let plain_dir = tempfile::tempdir().unwrap();
    let plain_store = DataFusionHist::open(plain_dir.path()).unwrap();
    let plain_total =
        ingest_local_file(&plain_store, &file_path, "2026-07-27", Stream::Book, None).unwrap();

    let bulk_dir = tempfile::tempdir().unwrap();
    let bulk_store = DataFusionHist::open(bulk_dir.path()).unwrap();
    let mut session = bulk_store.bulk_session(BulkConfig::default());
    let key_prefix = "vikearchive:book:2026-07-27:bulk";
    let bulk_staged = ingest_local_file_bulk(
        &mut session,
        &file_path,
        "2026-07-27",
        Stream::Book,
        None,
        key_prefix,
    )
    .unwrap();
    session.flush(key_prefix).unwrap();

    assert_eq!(plain_total, 2);
    assert_eq!(bulk_staged, 2, "bulk path decodes the same row count");
    let plain_rows = plain_store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap();
    let bulk_rows = bulk_store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap();
    assert_eq!(plain_rows.len(), bulk_rows.len(), "same committed row count either way");
    assert_eq!(bulk_rows.len(), 2);
}

/// A `BookUpdate` reduced to the fields that carry its content — `(ts, seq, bids, asks)`.
/// `BookUpdate` itself has no `PartialEq`, so equality assertions compare this instead.
type BookLens = (i64, u64, Vec<BookLevel>, Vec<BookLevel>);

/// Bulk + fan-out must commit exactly what serial bulk commits — across MANY tokens and MANY
/// row groups, so the work genuinely spreads over several chunks.
///
/// This is the test the whole per-worker key namespace exists for. With a shared key prefix
/// every worker's session would number its windows `w0`, `w1`, … from its own zero, minting the
/// SAME key for DIFFERENT rows; `commit_rows_bulk` treats a seen key as durably committed and
/// returns `Ok(0)`, so the second worker to reach a series would lose its rows silently. That
/// failure shows up here as a short row count, with no error anywhere.
#[test]
fn bulk_parallel_commits_exactly_what_serial_bulk_commits() {
    let ids = [
        "TOK_A", "TOK_B", "TOK_C", "TOK_D", "TOK_E", "TOK_F", "TOK_G", "TOK_H", "TOK_I", "TOK_J",
        "TOK_K", "TOK_L",
    ];
    // Two rows per token; one row group per row, so 24 groups fan across the workers and most
    // tokens straddle a group boundary (the contiguous-chunk property under real pressure).
    let rows: Vec<BookRow<'_>> = ids
        .iter()
        .enumerate()
        .flat_map(|(i, id)| {
            let ts = (i as i64 + 1) * 10;
            vec![
                (
                    *id,
                    ts,
                    ts,
                    1u64,
                    "book",
                    "none",
                    0.0,
                    0.0,
                    "[[0.5,10.0]]",
                    "[[0.51,10.0]]",
                    0.01,
                    "",
                ),
                (*id, ts + 1, ts + 1, 2u64, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
            ]
        })
        .collect();
    let bytes = write_book_events_parquet(&rows, 1);
    let file_dir = tempfile::tempdir().unwrap();
    let file_path = file_dir.path().join("book_events.parquet");
    std::fs::write(&file_path, &bytes).unwrap();
    let key_prefix = "vikearchive:book:2026-07-27:bulk";

    let serial_dir = tempfile::tempdir().unwrap();
    let serial_store = DataFusionHist::open(serial_dir.path()).unwrap();
    let mut session = serial_store.bulk_session(BulkConfig::default());
    let serial_staged = ingest_local_file_bulk(
        &mut session,
        &file_path,
        "2026-07-27",
        Stream::Book,
        None,
        key_prefix,
    )
    .unwrap();
    session.flush(key_prefix).unwrap();

    let par_dir = tempfile::tempdir().unwrap();
    let par_store = DataFusionHist::open(par_dir.path()).unwrap();
    let par_staged = ingest_local_file_bulk_parallel(
        &par_store,
        &file_path,
        "2026-07-27",
        Stream::Book,
        None,
        key_prefix,
        4,
        BulkConfig::default(),
    )
    .unwrap();

    assert_eq!(serial_staged, par_staged, "same rows decoded either way");
    // `BookUpdate` has no `PartialEq`, so compare on the fields that carry the content:
    // (ts, seq, levels). Both sides come back in the store's documented (ts, seq) order.
    let lens = |v: &[BookUpdate]| -> Vec<BookLens> {
        v.iter().map(|u| (u.ts, u.seq, u.bids.clone(), u.asks.clone())).collect()
    };
    for id in ids {
        let s = serial_store.scan_book_updates(VENUE, id, TsRange::all()).unwrap();
        let p = par_store.scan_book_updates(VENUE, id, TsRange::all()).unwrap();
        assert_eq!(
            lens(&s),
            lens(&p),
            "{id}: parallel bulk must commit exactly the serial bulk rows"
        );
        assert!(!p.is_empty(), "{id}: committed nothing — a dropped key would look like this");
    }
}

/// `--jobs 1` through the parallel bulk path is the serial bulk path: one chunk, one session,
/// file order. Guards the degenerate case so the bin can route on `jobs > 1` alone.
#[test]
fn bulk_parallel_with_one_job_equals_serial_bulk() {
    let rows: Vec<BookRow<'_>> = vec![
        ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
        ("TOK_A", 2, 2, 2, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
    ];
    let bytes = write_book_events_parquet(&rows, 1);
    let file_dir = tempfile::tempdir().unwrap();
    let file_path = file_dir.path().join("book_events.parquet");
    std::fs::write(&file_path, &bytes).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let staged = ingest_local_file_bulk_parallel(
        &store,
        &file_path,
        "2026-07-27",
        Stream::Book,
        None,
        "vikearchive:book:2026-07-27:bulk",
        1,
        BulkConfig::default(),
    )
    .unwrap();
    assert_eq!(staged, 2);
    assert_eq!(store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap().len(), 2);
}

// ---- chunk_row_groups: the fan-out unit's pure partitioning ----------------------------------

#[test]
fn chunk_row_groups_splits_as_evenly_as_possible_and_stays_contiguous_in_order() {
    let selected: Vec<usize> = (10..20).collect(); // 10 items, deliberately not 0-based
    let chunks = chunk_row_groups(&selected, 3);
    let lens: Vec<usize> = chunks.iter().map(Vec::len).collect();
    assert_eq!(lens, vec![4, 3, 3], "remainder goes to the first chunks");
    // Every original index appears exactly once, in order, across the concatenated chunks.
    let flat: Vec<usize> = chunks.into_iter().flatten().collect();
    assert_eq!(flat, selected, "contiguous split preserves file order end to end");
}

#[test]
fn chunk_row_groups_clamps_jobs_above_selected_len() {
    let selected = vec![5, 6, 7];
    let chunks = chunk_row_groups(&selected, 100);
    assert_eq!(chunks.len(), 3, "never more chunks than row groups");
    assert_eq!(chunks, vec![vec![5], vec![6], vec![7]]);
}

#[test]
fn chunk_row_groups_treats_zero_jobs_as_one() {
    let selected = vec![1, 2, 3];
    assert_eq!(chunk_row_groups(&selected, 0), vec![selected.clone()]);
    assert_eq!(chunk_row_groups(&selected, 1), vec![selected]);
}

#[test]
fn chunk_row_groups_empty_selection_is_no_chunks() {
    assert!(chunk_row_groups(&[], 8).is_empty());
}

// ---- ingest_local_file_parallel: must equal the serial path, INCLUDING under real cross-worker
// contention on a token whose two rows straddle a chunk boundary -----------------------------

/// A dozen distinct single-row tokens (one row group each, `max_rows_per_group=1`) ingested
/// once through the serial [`ingest_local_file`] and once through
/// [`ingest_local_file_parallel`] with `jobs=4` must land the identical row count AND the
/// identical per-symbol rows — the headline "parallel == serial" correctness proof the
/// measurement plan requires.
#[test]
fn ingest_local_file_parallel_matches_the_serial_path_row_counts_and_content() {
    let ids = [
        "TOK_A", "TOK_B", "TOK_C", "TOK_D", "TOK_E", "TOK_F", "TOK_G", "TOK_H", "TOK_I", "TOK_J",
        "TOK_K", "TOK_L",
    ];
    let rows: Vec<BookRow<'_>> = ids
        .iter()
        .enumerate()
        .map(|(i, sym)| {
            let ts = (i + 1) as i64;
            (
                *sym,
                ts,
                ts,
                1u64,
                "book",
                "none",
                0.0,
                0.0,
                "[[0.5,10.0]]",
                "[[0.51,10.0]]",
                0.01,
                "",
            )
        })
        .collect();
    let bytes = write_book_events_parquet(&rows, 1);
    let probe = ParquetRecordBatchReaderBuilder::try_new(bytes.clone()).unwrap();
    assert_eq!(probe.metadata().num_row_groups(), ids.len(), "one row group per token");

    let file_dir = tempfile::tempdir().unwrap();
    let file_path = file_dir.path().join("book_events.parquet");
    std::fs::write(&file_path, &bytes).unwrap();

    let serial_dir = tempfile::tempdir().unwrap();
    let serial_store = DataFusionHist::open(serial_dir.path()).unwrap();
    let serial_total =
        ingest_local_file(&serial_store, &file_path, "2026-07-28", Stream::Book, None).unwrap();

    let parallel_dir = tempfile::tempdir().unwrap();
    let parallel_store = DataFusionHist::open(parallel_dir.path()).unwrap();
    let parallel_total = ingest_local_file_parallel(
        &parallel_store,
        &file_path,
        "2026-07-28",
        Stream::Book,
        None,
        4,
    )
    .unwrap();

    assert_eq!(serial_total, ids.len());
    assert_eq!(parallel_total, serial_total, "parallel path writes the same row count");

    for sym in ids {
        let from_serial = serial_store.scan_book_updates(VENUE, sym, TsRange::all()).unwrap();
        let from_parallel = parallel_store.scan_book_updates(VENUE, sym, TsRange::all()).unwrap();
        assert_eq!(from_serial.len(), from_parallel.len(), "{sym}: same row count");
        assert_eq!(from_serial.len(), 1);
        assert_eq!(from_serial[0].ts, from_parallel[0].ts, "{sym}: ts must match");
    }
}

/// The parallel counterpart of
/// `ingest_stream_over_keeps_both_rows_when_one_symbol_spans_two_row_groups`: FOUR row groups
/// (one row each), `jobs=2` so [`chunk_row_groups`] hands worker 0 row groups `[0, 1]` and
/// worker 1 row groups `[2, 3]`. `TOK_A`'s two rows sit in row groups 1 and 2 — exactly
/// STRADDLING the chunk boundary — so this is the one case where two DIFFERENT worker threads
/// really do call `DataFusionHist::append_book_updates` for the SAME symbol concurrently,
/// exercising `SeriesLock`'s file-lock contention path for real (not just asserting the
/// design on paper). Both of `TOK_A`'s rows must survive — the store's per-series lock must
/// serialize the two concurrent manifest read-modify-writes correctly, not lose one.
#[test]
fn ingest_local_file_parallel_keeps_both_rows_when_one_symbol_spans_a_chunk_boundary() {
    let rows: Vec<BookRow<'_>> = vec![
        ("TOK_X", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.1,1.0]]", "[[0.11,1.0]]", 0.01, ""),
        ("TOK_A", 2, 2, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
        ("TOK_A", 3, 3, 2, "book", "none", 0.0, 0.0, "[[0.6,11.0]]", "[[0.61,11.0]]", 0.01, ""),
        ("TOK_Y", 4, 4, 1, "book", "none", 0.0, 0.0, "[[0.2,2.0]]", "[[0.21,2.0]]", 0.01, ""),
    ];
    let bytes = write_book_events_parquet(&rows, 1);
    let probe = ParquetRecordBatchReaderBuilder::try_new(bytes.clone()).unwrap();
    assert_eq!(probe.metadata().num_row_groups(), 4, "one row group per row");
    // Sanity-check the boundary this test relies on: jobs=2 over 4 groups -> [0,1] / [2,3].
    assert_eq!(chunk_row_groups(&[0, 1, 2, 3], 2), vec![vec![0, 1], vec![2, 3]]);

    let file_dir = tempfile::tempdir().unwrap();
    let file_path = file_dir.path().join("book_events.parquet");
    std::fs::write(&file_path, &bytes).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let total = ingest_local_file_parallel(&store, &file_path, "2026-07-28", Stream::Book, None, 2)
        .unwrap();

    assert_eq!(total, 4, "all four rows written — none dropped by the cross-worker boundary");
    let tok_a = store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap();
    assert_eq!(tok_a.len(), 2, "both of TOK_A's rows survive concurrent cross-worker writes");
    let tok_x = store.scan_book_updates(VENUE, "TOK_X", TsRange::all()).unwrap();
    let tok_y = store.scan_book_updates(VENUE, "TOK_Y", TsRange::all()).unwrap();
    assert_eq!(tok_x.len(), 1);
    assert_eq!(tok_y.len(), 1);
}

/// `jobs=1` must be byte-for-byte the same decode/commit order as the serial
/// [`ingest_local_file`] — a caller that wants the old behavior back gets it exactly, not just
/// "close enough". Uses a token spanning two row groups (like the serial-path test of the same
/// name) so a hypothetical broken single-chunk split would still be caught.
#[test]
fn ingest_local_file_parallel_with_jobs_one_matches_ingest_local_file_exactly() {
    let rows: Vec<BookRow<'_>> = vec![
        ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
        ("TOK_A", 2, 2, 2, "book", "none", 0.0, 0.0, "[[0.6,11.0]]", "[[0.61,11.0]]", 0.01, ""),
    ];
    let bytes = write_book_events_parquet(&rows, 1);
    let file_dir = tempfile::tempdir().unwrap();
    let file_path = file_dir.path().join("book_events.parquet");
    std::fs::write(&file_path, &bytes).unwrap();

    let serial_dir = tempfile::tempdir().unwrap();
    let serial_store = DataFusionHist::open(serial_dir.path()).unwrap();
    let serial_total =
        ingest_local_file(&serial_store, &file_path, "2026-07-27", Stream::Book, None).unwrap();

    let parallel_dir = tempfile::tempdir().unwrap();
    let parallel_store = DataFusionHist::open(parallel_dir.path()).unwrap();
    let parallel_total = ingest_local_file_parallel(
        &parallel_store,
        &file_path,
        "2026-07-27",
        Stream::Book,
        None,
        1,
    )
    .unwrap();

    assert_eq!(serial_total, 2);
    assert_eq!(parallel_total, 2);
    let from_serial = serial_store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap();
    let from_parallel = parallel_store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap();
    assert_eq!(from_serial.len(), 2);
    assert_eq!(from_parallel.len(), 2);
    for (a, b) in from_serial.iter().zip(from_parallel.iter()) {
        assert_eq!(a.ts, b.ts);
        assert_eq!(a.seq, b.seq);
    }
}

/// A token filter still prunes row groups (and drops unrelated rows within a kept group) under
/// the parallel path — mirrors `ingest_stream_over_respects_a_token_filter` for the serial core.
#[test]
fn ingest_local_file_parallel_respects_a_token_filter() {
    let rows: Vec<BookRow<'_>> = vec![
        ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
        ("TOK_B", 2, 2, 1, "book", "none", 0.0, 0.0, "[[0.4,20.0]]", "[[0.41,20.0]]", 0.01, ""),
    ];
    let bytes = write_book_events_parquet(&rows, 1);
    let file_dir = tempfile::tempdir().unwrap();
    let file_path = file_dir.path().join("book_events.parquet");
    std::fs::write(&file_path, &bytes).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let mut want = HashSet::new();
    want.insert("TOK_B".to_string());

    let total =
        ingest_local_file_parallel(&store, &file_path, "2026-07-27", Stream::Book, Some(&want), 2)
            .unwrap();
    assert_eq!(total, 1, "only TOK_B's row is written");
    assert!(store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap().is_empty());
    assert_eq!(store.scan_book_updates(VENUE, "TOK_B", TsRange::all()).unwrap().len(), 1);
}

// ---- HttpRangeReader: the get_bytes buffer is bounded whatever the server claims ------------
//
// These drive a LOOPBACK origin that lies, because the whole point of the cap is that the two
// numbers parquet cross-checks — the footer's `metadata_len` and `ChunkReader::len()` — both
// come from the server. Every assertion is on bytes the client actually BUFFERED (the returned
// `Bytes`) or on whether a request was dialled at all, never merely on "an error came back":
// the pre-cap code also returned an error in two of these three cases, just after allocating.

/// A minimal HTTP/1.1 origin on loopback. `HEAD` answers `claimed_len` as `Content-Length` —
/// the number a hostile or misconfigured host controls, and the ONLY thing parquet's own footer
/// validation checks a `get_bytes` length against. `GET` parses `Range: bytes=A-B` and replies
/// `206` with whatever `body_for(a, b)` returns, which the caller is free to make SHORTER or
/// LONGER than the range asked for. `gets` counts dialled range requests, so a test can assert
/// that a refusal happened before any socket was opened.
struct LyingOrigin {
    url: String,
    gets: Arc<AtomicUsize>,
}

fn spawn_lying_origin(
    claimed_len: u64,
    body_for: impl Fn(u64, u64) -> Vec<u8> + Send + Sync + 'static,
) -> LyingOrigin {
    use std::io::Write as _;
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().unwrap().port();
    let gets = Arc::new(AtomicUsize::new(0));
    let gets_srv = Arc::clone(&gets);

    // Daemon thread: the test process exits without joining it. Each connection is answered
    // once and closed (`Connection: close`), so there is no keep-alive state to get wrong.
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut sock) = stream else { break };
            // Read just the request head — everything up to the blank line.
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match sock.read(&mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => break,
                }
            }
            let head = String::from_utf8_lossy(&head).to_string();
            if head.starts_with("HEAD ") {
                let _ = write!(
                    sock,
                    "HTTP/1.1 200 OK\r\nContent-Length: {claimed_len}\r\n\
                         Accept-Ranges: bytes\r\nConnection: close\r\n\r\n"
                );
                continue;
            }
            gets_srv.fetch_add(1, Ordering::SeqCst);
            // `Range: bytes=A-B` — the reader always sends an explicit closed range.
            let (a, b) = head
                .split("bytes=")
                .nth(1)
                .and_then(|r| r.split_whitespace().next())
                .and_then(|r| r.split_once('-'))
                .and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?)))
                .unwrap_or((0u64, 0u64));
            let body = body_for(a, b);
            let _ = write!(
                sock,
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {a}-{b}/{claimed_len}\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            // The over-send case abandons this write once the client stops reading; a broken
            // pipe here is the EXPECTED outcome, not a test failure.
            let _ = sock.write_all(&body);
        }
    });

    LyingOrigin { url: format!("http://127.0.0.1:{port}/book_events.parquet"), gets }
}

fn test_agent() -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).build().new_agent()
}

/// A server that answers a small `Range` with a MUCH larger body cannot grow the buffer past
/// what was asked for. This is the case `Read::take` exists for, and the one that most cleanly
/// separates the two implementations: with the bare `read_to_end` this test buffered all 8 MiB
/// and then failed the `buf.len() != length` check, so it would have gone RED on the exact
/// symptom (an 8 MiB allocation for a 4 KiB request) rather than on a cosmetic difference.
#[test]
fn get_bytes_stops_at_the_requested_length_when_the_server_over_sends() {
    const ASKED: usize = 4096;
    const OFFERED: usize = 8 * 1024 * 1024;
    let origin = spawn_lying_origin(64 * 1024 * 1024, |_a, _b| vec![0xABu8; OFFERED]);
    let reader = HttpRangeReader::open(test_agent(), origin.url.clone(), None)
        .expect("HEAD the lying origin");

    let got = reader.get_bytes(0, ASKED).expect("a bounded read still succeeds");

    // The direct measurement of bytes buffered: exactly what we asked for, out of 8 MiB
    // offered. 2048x less than the server was willing to hand over.
    assert_eq!(got.len(), ASKED, "buffer must be bounded by the requested length");
    assert!(got.iter().all(|b| *b == 0xAB), "and it is the server's actual bytes");
}

/// The opposite failure — a server that claims more than it sends — must still be a clean,
/// bounded `Err`, and must not be papered over by the `take` above into a silently short slice.
#[test]
fn get_bytes_errors_bounded_when_the_server_under_sends() {
    const ASKED: usize = 4096;
    const SENT: usize = 100;
    let origin = spawn_lying_origin(64 * 1024 * 1024, |_a, _b| vec![0x11u8; SENT]);
    let reader = HttpRangeReader::open(test_agent(), origin.url.clone(), None)
        .expect("HEAD the lying origin");

    let err = reader.get_bytes(0, ASKED).expect_err("a short body is an error");

    assert_matches!(err, ParquetError::EOF(_), "short body is EOF, not General: {err}");
    // The error names how much actually arrived — i.e. the buffer stopped at the 100 bytes the
    // server sent, not at the 4096 it claimed.
    let msg = err.to_string();
    assert!(msg.contains("expected 4096 bytes, got 100"), "unexpected message: {msg}");
}

/// An absurd `Content-Length` is refused BEFORE anything is allocated or dialled. `gets == 0`
/// is the assertion that matters: it proves the cap short-circuits ahead of
/// `Vec::with_capacity`, which on a length this size does not return an `Err` at all — it calls
/// `handle_alloc_error` and aborts the process.
#[test]
fn get_bytes_refuses_an_absurd_length_before_allocating_or_dialling() {
    let absurd = MAX_CHUNK_BYTES + 1;
    let origin = spawn_lying_origin(1 << 40, |_a, _b| Vec::new());
    let reader = HttpRangeReader::open(test_agent(), origin.url.clone(), None)
        .expect("HEAD the lying origin");
    assert_eq!(reader.len(), 1 << 40, "the reader believes the server's Content-Length");

    let err = reader.get_bytes(0, absurd as usize).expect_err("must refuse");

    assert_eq!(origin.gets.load(Ordering::SeqCst), 0, "no request may be dialled");
    let msg = err.to_string();
    assert!(msg.contains("refusing"), "unexpected message: {msg}");
    assert!(msg.contains(&MAX_CHUNK_BYTES.to_string()), "message must name the cap: {msg}");
}

/// The end-to-end shape this cap exists for: a hostile FILE, not a hostile caller. The last 8
/// bytes of a Parquet file are `[metadata_len: u32 LE][b"PAR1"]`, and
/// `ParquetMetaDataReader::parse_metadata` accepts any `metadata_len` that fits inside
/// `ChunkReader::len()` — which here is the server's own `Content-Length`. A host controlling
/// both therefore walks parquet straight into `get_bytes(_, 4_294_967_295)`. Without the cap
/// that is a 4 GiB `Vec::with_capacity`; with it, a clean `Err` and no second request.
#[test]
fn a_hostile_footer_cannot_drive_a_multi_gigabyte_read() {
    let claimed_len: u64 = 5_000_000_000;
    let mut footer = Vec::new();
    footer.extend_from_slice(&u32::MAX.to_le_bytes()); // metadata_len = 4_294_967_295
    footer.extend_from_slice(b"PAR1");
    let origin = spawn_lying_origin(claimed_len, move |a, b| {
        // Only the footer tail is ever served; any other range gets nothing, which is enough
        // because the cap must fire on the very next call.
        if a == claimed_len - 8 && b == claimed_len - 1 { footer.clone() } else { Vec::new() }
    });
    let reader = HttpRangeReader::open(test_agent(), origin.url.clone(), None)
        .expect("HEAD the lying origin");

    let err = ParquetRecordBatchReaderBuilder::try_new(reader)
        .err()
        .expect("a 4 GiB footer must not be honoured");

    let msg = err.to_string();
    assert!(msg.contains("refusing"), "the cap must be what stopped it, got: {msg}");
    assert!(msg.contains("4294967295"), "and it must name the refused size: {msg}");
    // Exactly one range GET: the 8-byte footer tail. The 4 GiB follow-up never left the process.
    assert_eq!(origin.gets.load(Ordering::SeqCst), 1, "the huge read was never dialled");
}

// ---- live network smokes (never run in CI; manual only) --------------------------------------

/// Proves ranged reads actually work end-to-end against the REAL archive: opens the free,
/// keyless daily sample (`/archive/samples/book_events.parquet`, no `X-API-Key` needed) with
/// [`HttpRangeReader`], reads the footer-only metadata, and decodes ONE row group. Always
/// `#[ignore]`d — needs network, which unit tests must not require — run manually:
/// `cargo test -p vike-backfill --features vike-archive --lib vike_archive::tests::live_range_read_against_free_sample -- --ignored --nocapture`.
#[test]
#[ignore]
fn live_range_read_against_free_sample() {
    let agent = ureq::Agent::config_builder().http_status_as_error(false).build().new_agent();
    let url = "https://data.vike.io/archive/samples/book_events.parquet".to_string();
    let reader = HttpRangeReader::open(agent, url, None).expect("HEAD the free sample");
    assert!(reader.len() > 0);
    let builder = ParquetRecordBatchReaderBuilder::try_new(reader).expect("footer-only open");
    assert!(builder.metadata().num_row_groups() >= 1);
    let mut reader = builder.build().unwrap();
    let batch = reader.next().expect("at least one batch").unwrap();
    let decoded = book_updates_from_batch(&batch).expect("decode the real schema");
    println!("live sample: decoded {} book updates from one batch", decoded.len());
}

/// Proves the manifest + a keyed per-date footer-only plan against the REAL archive.
/// Double-gated (network + `VIKE_ARCHIVE_API_KEY`): self-skips when the key is absent — the
/// same idiom the venue demo smokes use. Run manually:
/// `cargo test -p vike-backfill --features vike-archive --lib vike_archive::tests::live_manifest_and_plan -- --ignored --nocapture`.
#[test]
#[ignore]
fn live_manifest_and_plan() {
    let Some(key) = std::env::var("VIKE_ARCHIVE_API_KEY").ok() else {
        eprintln!("VIKE_ARCHIVE_API_KEY not set — skipping live archive smoke");
        return;
    };
    let client = ArchiveClient::new(DEFAULT_BASE, Some(key));
    let manifest = client.fetch_manifest().expect("fetch the real manifest");
    assert!(!manifest.dates.is_empty());
    let date = &manifest.dates[0].date;
    let mut want = HashSet::new();
    // An arbitrary sample token id seen in the free sample fixture — a real archive run would
    // narrow to a token the operator actually cares about.
    want.insert(
        "7715333804644496306161804929604508138595580668391178416151948416889614694288".to_string(),
    );
    let plan = client.plan_stream(date, Stream::Book, Some(&want)).expect("plan a real date");
    println!(
        "live plan {date}/book_events: {}/{} row groups, {} / {} compressed bytes",
        plan.selected_row_groups,
        plan.total_row_groups,
        plan.selected_compressed_bytes,
        plan.total_compressed_bytes
    );
}

/// Proves the real `/v1/archive/datasets` + `/v1/archive/url` discovery routes end-to-end
/// against the live production host, and that the URL they hand back is directly openable by
/// [`HttpRangeReader`] (never re-derived by this client). Double-gated (network +
/// `VIKE_ARCHIVE_API_KEY`): self-skips when the key is absent. Run manually:
/// `cargo test -p vike-backfill --features vike-archive --lib vike_archive::tests::live_discover_and_resolve_btc_5m -- --ignored --nocapture`.
#[test]
#[ignore]
fn live_discover_and_resolve_btc_5m() {
    let Some(key) = std::env::var("VIKE_ARCHIVE_API_KEY").ok() else {
        eprintln!("VIKE_ARCHIVE_API_KEY not set — skipping live discovery smoke");
        return;
    };
    let client = ArchiveClient::new(DEFAULT_BASE, Some(key.clone()));
    let discovery_base = default_discovery_base(DEFAULT_BASE);

    let filter = DatasetFilter {
        asset: Some("btc".to_string()),
        tenor: Some("5m".to_string()),
        ..Default::default()
    };
    let page = client.discover_datasets(&discovery_base, &filter).expect("discover btc/5m");
    assert!(!page.datasets.is_empty(), "expected at least one btc/5m dataset row");
    assert!(page.datasets.iter().all(|d| d.layout == "family"));
    let date = page.datasets[0].date.clone();

    let resolved = client
        .resolve_dataset(&discovery_base, &date, Stream::Book, Some("btc"), Some("5m"))
        .expect("resolve one btc/5m dataset");
    assert_eq!(resolved.asset.as_deref(), Some("btc"));
    assert_eq!(resolved.tenor.as_deref(), Some("5m"));

    // The resolved URL must be directly openable — prove it with a real ranged HEAD/GET,
    // exactly like `live_range_read_against_free_sample` does for the flat/keyless sample.
    let agent = ureq::Agent::config_builder().http_status_as_error(false).build().new_agent();
    let reader = HttpRangeReader::open(agent, resolved.url.clone(), Some(key))
        .expect("open the discovered family URL");
    assert!(reader.len() > 0);
    println!(
        "live discovery: {date}/book_events resolved to {} ({} bytes, layout={})",
        resolved.url, resolved.bytes, resolved.layout
    );
}
