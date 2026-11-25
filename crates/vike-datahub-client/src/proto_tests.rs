use std::io::{self, Cursor};

use super::*;
use vike_hub_protocol::features::{
    FEATURE_MARKET_DATA_VENUE_PREFIX, advertised_market_data_venues, market_data_venue_feature,
};
use vike_hub_protocol::frame::{read_frame, read_frame_raw, write_frame};
use vike_model::StoreKindId;

/// The raw read returns a well-framed body WITHOUT decoding, so an undecodable body is
/// recoverable (a server answers it with `Response::Error`); `read_frame` on the SAME bytes still
/// errors on decode. This is the decode-vs-drop split the server relies on.
#[test]
fn read_frame_raw_separates_framing_from_decode() {
    // Valid JSON, but not a known `Request` variant: externally-tagged unit variants serialize
    // as JSON strings, and `"Bogus"` is not one of them, so it frames fine yet fails to decode.
    let body: &[u8] = b"\"Bogus\"";
    let mut framed: Vec<u8> = u32::try_from(body.len()).unwrap().to_be_bytes().to_vec();
    framed.extend_from_slice(body);

    // raw: returns the body bytes verbatim, no decode
    let mut cursor = Cursor::new(framed.clone());
    let raw = read_frame_raw(&mut cursor).unwrap();
    assert_eq!(raw.as_slice(), body, "raw read returns the framed body untouched");

    // fused: the same bytes fail to decode into a `Request`
    let mut current2 = Cursor::new(framed);
    let err = read_frame::<_, Request>(&mut current2).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData, "an unknown variant fails to decode");
}

/// `read_frame_raw` keeps `read_frame`'s OOM guard: an oversized declared length is rejected
/// before any body allocation.
#[test]
fn read_frame_raw_rejects_an_oversized_length() {
    let mut framed = (MAX_FRAME_LEN + 1).to_be_bytes().to_vec();
    framed.push(0); // a trailing byte — the guard must fire on the length, not on a short read
    let mut cursor = Cursor::new(framed);
    let err = read_frame_raw(&mut cursor).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

/// The `Backfill` request and `BackfillDone` response (the split-plane backfill-on-demand
/// verb) survive `write_frame` -> `read_frame` over an in-memory buffer.
#[test]
fn backfill_and_backfill_done_survive_the_frame_codec() {
    let mut buffer: Vec<u8> = Vec::new();
    write_frame(
        &mut buffer,
        &Request::Backfill {
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            interval: "1h".to_string(),
            start: 1_000,
            end: 2_000,
        },
    )
    .unwrap();
    let done =
        BackfillDone { rows_written: 5, first_timestamp: Some(1_000), last_timestamp: Some(1_900) };
    write_frame(&mut buffer, &Response::BackfillDone(done.clone())).unwrap();

    let mut cursor = Cursor::new(buffer);
    match read_frame::<_, Request>(&mut cursor).unwrap() {
        Request::Backfill { venue, symbol, interval, start, end } => {
            assert_eq!(venue, "binance");
            assert_eq!(symbol, "BTCUSDT");
            assert_eq!(interval, "1h");
            assert_eq!(start, 1_000);
            assert_eq!(end, 2_000);
        }
        other => panic!("expected Backfill, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cursor).unwrap() {
        Response::BackfillDone(got) => assert_eq!(got, done),
        other => panic!("expected BackfillDone, got {other:?}"),
    }
}

/// The no-rows outcome (an already-ingested or empty range) round-trips with BOTH ts fields
/// `None` — distinguishable from a failure, which is `Response::Error`, never a zeroed
/// `BackfillDone`.
#[test]
fn backfill_done_no_rows_shape_round_trips() {
    let done = BackfillDone { rows_written: 0, first_timestamp: None, last_timestamp: None };
    let mut buffer: Vec<u8> = Vec::new();
    write_frame(&mut buffer, &Response::BackfillDone(done.clone())).unwrap();
    let mut cursor = Cursor::new(buffer);
    match read_frame::<_, Response>(&mut cursor).unwrap() {
        Response::BackfillDone(got) => assert_eq!(got, done),
        other => panic!("expected BackfillDone, got {other:?}"),
    }
}

/// The `Coverage` request and a NON-EMPTY `Coverage` response survive the frame codec with
/// every field intact — the `BTreeMap<String, InstrumentCoverageKindDays>` and its day vectors included.
///
/// The payload is built through `vike_data::store::coverage::join_coverage` rather than by hand, so
/// this pins the REAL shape the store produces (every `TICK_KINDS` entry present, absent kinds
/// as empty rows) rather than a hand-written approximation of it.
#[test]
fn coverage_request_and_a_non_empty_report_survive_the_frame_codec() {
    let report = vike_data::store::coverage::join_coverage(&[
        (
            vike_data::SeriesId::per_symbol(StoreKindId::Trade, "binance", "BTCUSDT", None),
            vec![19_000, 19_001, 19_003],
        ),
        (
            vike_data::SeriesId::per_symbol(StoreKindId::Quote, "binance", "BTCUSDT", None),
            vec![19_000, 19_001],
        ),
    ]);
    assert!(!report.is_empty(), "the fixture must carry a real entry, not an empty vec");

    let mut buffer: Vec<u8> = Vec::new();
    write_frame(&mut buffer, &Request::Coverage).unwrap();
    write_frame(&mut buffer, &Response::Coverage(report.clone())).unwrap();

    let mut cursor = Cursor::new(buffer);
    match read_frame::<_, Request>(&mut cursor).unwrap() {
        Request::Coverage => {}
        other => panic!("expected Coverage, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cursor).unwrap() {
        Response::Coverage(got) => {
            assert_eq!(got, report, "the report survives the codec whole");
            // ...and the DERIVED answer the Data Manager renders survives with it: the gap in
            // the trade days and the quote kind's shorter span both have to make it across for
            // this to hold.
            assert_eq!(
                got[0].partial_days(),
                report[0].partial_days(),
                "the partial-day fold is identical on both sides of the codec"
            );
        }
        other => panic!("expected Coverage, got {other:?}"),
    }
}

/// The `build=` pair ROUND-TRIPS, and a datahub that states no build (an older one) or an empty one reads as none: the
/// backtesthub then falls back to the advertised features.
#[test]
fn the_build_feature_round_trips_and_its_absence_reads_as_none() {
    let features =
        vec![FEATURE_MARKET_DATA.to_string(), build_feature("ab12"), rec_venue_feature("x")];
    assert_eq!(advertised_build(&features).as_deref(), Some("ab12"));
    assert_eq!(advertised_build(&[FEATURE_MARKET_DATA.to_string()]), None, "an older datahub");
    assert_eq!(advertised_build(&[build_feature(" ")]), None, "an empty value");
}

/// The `rec_venue=` pair ROUND-TRIPS, which is the whole reason the builder and the reader ship
/// together: the server writes through one and the client reads through the other, so the
/// spelling exists once.
///
/// Modelled on `crates/vike-datahub-client/tests/market_data_negotiation.rs`'s
/// `the_market_data_venue_feature_round_trips` — the twin this pair was built in the image of.
#[test]
fn the_rec_venue_feature_round_trips() {
    let features = vec![
        rec_venue_feature("binance"),
        FEATURE_MARKET_DATA.to_string(),
        rec_venue_feature("polymarket"),
    ];
    assert_eq!(
        advertised_rec_venues(&features),
        vec!["binance".to_string(), "polymarket".to_string()],
        "advertisement ORDER is the contract, and a named capability between two entries is \
             not one of them"
    );
}

/// The two per-venue prefixes describe DIFFERENT PLANES and neither reader may see the other's
/// entries — the defect that would make a client report a venue as recordable because it
/// happens to be servable (the shipped image serves six and records two).
///
/// Also the EMPTY-value and whole-string-equality rules, which are
/// [`FEATURE_REC_VENUE_PREFIX`]'s own claims.
#[test]
fn the_two_venue_planes_advertise_separately() {
    let both = vec![market_data_venue_feature("okx"), rec_venue_feature("binance")];
    assert_eq!(advertised_rec_venues(&both), vec!["binance".to_string()]);
    assert_eq!(advertised_market_data_venues(&both), vec!["okx".to_string()]);

    // An empty or blank value advertises nothing — "absence is the answer".
    assert!(advertised_rec_venues(&[FEATURE_REC_VENUE_PREFIX.to_string()]).is_empty());
    assert!(advertised_rec_venues(&["rec_venue=   ".to_string()]).is_empty());
    assert_eq!(
        advertised_rec_venues(&["rec_venue= bybit ".to_string()]),
        vec!["bybit".to_string()],
        "the value is trimmed, exactly as the market data twin trims"
    );

    // A NAMED capability can neither satisfy nor shadow a per-venue entry.
    let named = vec![FEATURE_BACKFILL.to_string(), FEATURE_COVERAGE.to_string()];
    assert!(advertised_rec_venues(&named).is_empty());
    assert!(advertised_rec_venues(&[]).is_empty(), "no entries is an EMPTY answer, not a panic");
}

/// The `import_format=` pair ROUND-TRIPS in advertisement order, the shape its two venue-prefix
/// precedents established — and it reads ONE prefix: a venue entry of either plane, and the named
/// `archive_import` capability itself, are not formats.
#[test]
fn the_import_format_feature_round_trips_and_reads_one_prefix() {
    let features = vec![
        FEATURE_ARCHIVE_IMPORT.to_string(),
        import_format_feature("dukascopy-bi5"),
        market_data_venue_feature("okx"),
        rec_venue_feature("binance"),
        import_format_feature("tardis-csv"),
    ];
    assert_eq!(
        advertised_import_formats(&features),
        vec!["dukascopy-bi5".to_string(), "tardis-csv".to_string()],
        "advertisement ORDER is the contract, and nothing but the prefix is read"
    );
    assert_eq!(advertised_market_data_venues(&features), vec!["okx".to_string()]);
    assert_eq!(advertised_rec_venues(&features), vec!["binance".to_string()]);

    // An empty or blank value advertises nothing — "absence is the answer".
    assert!(advertised_import_formats(&[FEATURE_IMPORT_FORMAT_PREFIX.to_string()]).is_empty());
    assert!(advertised_import_formats(&["import_format=  ".to_string()]).is_empty());
    // Whole-string equality: the named capability is not a format, and a format entry is not the
    // capability.
    assert!(advertised_import_formats(&[FEATURE_ARCHIVE_IMPORT.to_string()]).is_empty());
    assert!(!features[1..].iter().any(|f| f == FEATURE_ARCHIVE_IMPORT));
    assert_ne!(FEATURE_IMPORT_FORMAT_PREFIX, FEATURE_MARKET_DATA_VENUE_PREFIX);
    assert_ne!(FEATURE_IMPORT_FORMAT_PREFIX, FEATURE_REC_VENUE_PREFIX);
}

/// Both answers survive the frame codec, the one-batch reason is the text the design names, and
/// the capability is its own whole string — not `backfill` and not a prefix of it.
#[test]
fn the_backfill_registry_answers_round_trip_and_the_capability_is_its_own_string() {
    let running = BackfillRunWire {
        id: 7,
        venue: "oanda".to_string(),
        symbol: "EUR_USD".to_string(),
        interval: "5s".to_string(),
        start: 1_104_710_400_000,
        end: 1_104_796_799_999,
        peer: Some("127.0.0.1:50000".to_string()),
        started_ms: 1_700_000_000_000,
        elapsed_ms: 12_345,
        lane: "CredentialedKlines".to_string(),
        stoppable: true,
        cancelled: false,
        boundaries: 3,
    };
    let one_batch = BackfillRunWire {
        id: 8,
        lane: "Klines".to_string(),
        stoppable: false,
        boundaries: 0,
        ..running.clone()
    };
    let done =
        BackfillCancelDone { flagged: vec![running.clone()], unstoppable: vec![one_batch.clone()] };
    let mut buffer: Vec<u8> = Vec::new();
    write_frame(&mut buffer, &Response::RunningBackfills(vec![running.clone(), one_batch]))
        .unwrap();
    write_frame(&mut buffer, &Response::BackfillsCancelled(done.clone())).unwrap();
    let mut cursor = Cursor::new(buffer);
    match read_frame::<_, Response>(&mut cursor).unwrap() {
        Response::RunningBackfills(got) => assert_eq!(got.len(), 2),
        other => panic!("expected RunningBackfills, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cursor).unwrap() {
        Response::BackfillsCancelled(got) => assert_eq!(got, done),
        other => panic!("expected BackfillsCancelled, got {other:?}"),
    }
    assert_eq!(BACKFILL_ONE_BATCH, "cannot stop: one batch per request");
    assert_ne!(FEATURE_BACKFILL_CANCEL, FEATURE_BACKFILL);
    assert!(!FEATURE_BACKFILL_CANCEL.is_empty());
    let features = [FEATURE_BACKFILL.to_string()];
    assert!(
        !features.iter().any(|f| f == FEATURE_BACKFILL_CANCEL),
        "a `backfill` advertisement must not read as the cancel capability"
    );
}

/// ⚠ `ScanExecFills` WITHOUT a window is BYTE-IDENTICAL to the frame the variant had before
/// `start`/`end`/`limit` existed — the three are skipped when `None` — so an old server sees the
/// old request; and a frame an OLD client wrote (no such keys) still decodes, all three `None`.
#[test]
fn scan_execution_fills_without_a_range_is_byte_identical_to_the_old_frame() {
    let r = Request::ScanExecFills {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        start: None,
        end: None,
        limit: None,
        columns: None,
    };
    let old = r#"{"ScanExecFills":{"venue":"binance","symbol":"BTCUSDT"}}"#;
    assert_eq!(serde_json::to_string(&r).unwrap(), old);
    match serde_json::from_str::<Request>(old).unwrap() {
        Request::ScanExecFills { start, end, limit, .. } => {
            assert_eq!((start, end, limit), (None, None, None));
        }
        other => panic!("expected ScanExecFills, got {other:?}"),
    }
    // …and a windowed one carries the three keys
    let windowed = Request::ScanExecFills {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        start: Some(1),
        end: Some(2),
        limit: Some(3),
        columns: None,
    };
    let v = serde_json::to_value(&windowed).unwrap();
    assert_eq!(v["ScanExecFills"]["start"], 1);
    assert_eq!(v["ScanExecFills"]["end"], 2);
    assert_eq!(v["ScanExecFills"]["limit"], 3);
    assert_eq!(FEATURE_SCAN_EXECUTION_FILLS_RANGE, "scan_exec_fills_range");
}

/// `ScanExecOrders` and its `ExecOrders` reply survive the frame codec, and the three exec
/// capability strings are their own spellings.
#[test]
fn scan_execution_orders_request_and_response_round_trip() {
    let mut buffer: Vec<u8> = Vec::new();
    let req = Request::ScanExecOrders {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        start: Some(10),
        end: None,
        limit: Some(500),
        columns: None,
    };
    write_frame(&mut buffer, &req).unwrap();
    let row = vike_data::ExecOrderRow {
        ts: 10,
        order_client_id: "c1".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        order_type: "limit".to_string(),
        status: "FILLED".to_string(),
        price: Some(100.0),
        trigger_price: None,
        venue_order_id: Some("v1".to_string()),
        filled_qty: 1.0,
        avg_fill_px: 100.0,
        account: Some("ALT".to_string()),
        mount_id: None,
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
        reason: None,
    };
    write_frame(&mut buffer, &Response::ExecOrders(vec![row.clone()])).unwrap();
    let mut cursor = Cursor::new(buffer);
    match read_frame::<_, Request>(&mut cursor).unwrap() {
        Request::ScanExecOrders { venue, start, end, limit, .. } => {
            assert_eq!((venue.as_str(), start, end, limit), ("binance", Some(10), None, Some(500)));
        }
        other => panic!("expected ScanExecOrders, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cursor).unwrap() {
        Response::ExecOrders(rows) => assert_eq!(rows, vec![row]),
        other => panic!("expected ExecOrders, got {other:?}"),
    }
    assert_eq!(FEATURE_SCAN_EXECUTION_ORDERS, "scan_exec_orders");
    assert_eq!(FEATURE_EXECUTIONS_RECORDED, "exec_materialize");
}

/// **A full `Records` frame fits the stream's ceiling, measured**: a report with an order and its reason, a fill and a fee
/// correction, every one of its fifteen strings at [`EXECUTION_STREAM_STRING_CEILING_BYTES`] of quotes (each
/// escaped to two bytes) and every number at its widest, encodes under [`EXECUTION_STREAM_WORST_RECORD_JSON_BYTES`],
/// and [`EXECUTION_STREAM_RECORDS_PER_FRAME`] of them frame under [`EXECUTION_STREAM_FRAME_CEILING_BYTES`].
#[test]
fn a_full_records_frame_at_the_string_ceilings_fits_the_frame_ceiling() {
    use crate::execution_stream::*;
    let text = || "\"".repeat(EXECUTION_STREAM_STRING_CEILING_BYTES);
    let widest = -f64::MIN_POSITIVE;
    let record = ExecutionRecordWire {
        sequence: u64::MAX,
        previous_sequence: Some(u64::MAX),
        recorded_ms: i64::MIN,
        body: ExecutionRecordBodyWire::Report(Box::new(ExecutionReportWire {
            caused_by_sequence: u64::MAX,
            at_ms: i64::MIN,
            ownership: vike_model::orders::ExecutionOwnership::Foreign,
            account: Some(text()),
            venue: text(),
            symbol: text(),
            order_client_id: text(),
            venue_order_id: Some(text()),
            mount_id: Some(text()),
            order: Some(ExecutionReportOrderWire {
                side: i32::MIN,
                qty: widest,
                order_type: text(),
                status: text(),
                price: Some(widest),
                trigger_price: Some(widest),
                filled_qty: widest,
                average_fill_price: widest,
                reason: Some(text()),
            }),
            fill: Some(ExecutionReportFillWire {
                trade_id: text(),
                side: i32::MIN,
                qty: widest,
                price: widest,
                commission: widest,
                commission_asset: text(),
                liquidity_side: text(),
                mark_price: Some(widest),
                position_side: Some(text()),
            }),
            fee_correction: Some(ExecutionReportFeeCorrectionWire {
                corrects_trade_id: text(),
                commission: widest,
                commission_asset: text(),
                price: widest,
            }),
        })),
    };
    let one = serde_json::to_vec(&record).expect("encodes").len();
    assert!(one <= EXECUTION_STREAM_WORST_RECORD_JSON_BYTES, "{one} bytes");
    let frame = ExecutionStreamUp::Records(vec![record; EXECUTION_STREAM_RECORDS_PER_FRAME]);
    let mut wire = Vec::new();
    write_frame(&mut wire, &frame).expect("a full frame encodes");
    assert!(wire.len() <= EXECUTION_STREAM_FRAME_CEILING_BYTES as usize, "{} bytes", wire.len());
}
